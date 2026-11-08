#!/usr/bin/env node
/**
 * Writes a realistic reports folder for the Model Scoreboard's tests and page.
 *
 *   node scripts/scoreboard-fixture.mjs <reports-dir>
 *
 * <reports-dir> gets v1/catalog/ (the packaged seeds) and v1/writers/<writer>/runs/<date>/: several
 * models, runtimes and efforts across task types, observed and benchmark runs, blind reviews
 * (including an overturn, a disputed overturn and a cannot-verify), conduct flags with evidence, an
 * imported run, Scout runs keyed by process, rows below minRuns, a rewritten run (highest seq wins),
 * a review of an unknown run, runs the catalog does not know, one damaged file and one temporary file.
 * It may share a folder with scripts/status-fixture.mjs; it refuses a folder that already has a
 * catalog. Times are relative to now (or `now`), so run it just before the test.
 *
 * Start the test server with AAC_AGENT_REPORTS_DIR=<reports-dir>.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HOUR = 3_600_000;
const SEEDS = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  'docs/agent-reports/catalog'
);

const LEAD = 'vm.scoreboard-lead';
const REVIEWS = 'mac.scoreboard-reviews';
const BENCH = 'windows.scoreboard-bench';

const OPUS = {
  provider: 'anthropic',
  id: 'claude-opus-5-5',
  harness: 'claude-code',
  effort: 'xhigh',
  account: 'account-a',
};
const OPUS_HIGH = { ...OPUS, effort: 'high' };
// Written as the planning benchmark writes it: the id already starts with the provider.
const SOL = {
  provider: 'openai',
  id: 'openai/gpt-6.1-sol',
  harness: 'codex',
  effort: 'high',
  account: 'account-b',
};
const SOL_MEDIUM = { ...SOL, effort: 'medium' };
const GLM = {
  provider: 'zai',
  id: 'glm-5.3',
  harness: 'zcode',
  effort: 'max',
  account: 'account-c',
};
const SONNET = {
  provider: 'anthropic',
  id: 'claude-sonnet-5-5',
  harness: 'claude-code',
  effort: null,
  account: 'account-a',
};
const HAIKU = {
  provider: 'anthropic',
  id: 'claude-haiku-5-5',
  harness: 'claude-code',
  effort: null,
  account: 'account-a',
};
const MUSE = {
  provider: 'muse',
  id: 'muse-spark-1.3-contributor',
  harness: 'muse',
  effort: 'max',
  account: 'account-d',
};
const UNLISTED = {
  provider: 'example',
  id: 'example-1',
  harness: 'omp',
  effort: null,
  account: null,
};

function stamp(ms) {
  return new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z');
}

/** Run documents, collected in order; each `at` is hours before now. */
function makeRuns(now) {
  const runs = [];
  const add = (writer, local, fields) => {
    const { at, type, task, title, size, wall = 600, model, ...rest } = fields;
    const ended = now - at * HOUR;
    const doc = {
      schema: 'agent-reports/run@1',
      id: `${writer}--${local}`,
      writer,
      seq: 1,
      writtenAt: stamp(ended + 60_000),
      source: 'observed',
      project: 'example-project',
      stream: 'S',
      task: { id: task, type, title: title ?? `${type ?? 'Unknown work'} ${task}`, size },
      model,
      startedAt: stamp(ended - wall * 1000),
      endedAt: stamp(ended),
      wallSeconds: wall,
      outcome: 'done',
      flags: [],
      evidence: [],
      ...rest,
    };
    runs.push(doc);
    return doc.id;
  };
  const review = (local, of, at, model, verdict = {}) =>
    add(REVIEWS, local, {
      at,
      type: 'review',
      task: local,
      title: `Blind review of ${of}`,
      size: { records: 40 },
      wall: verdict.wall ?? 300,
      model,
      cost: { credits: 2, usd: null, quota: null, tokens: null },
      scope: { filesAssigned: 10, filesRead: 10, recordsAssigned: 40, recordsJudged: 40 },
      review: {
        of,
        verdict: verdict.verdict ?? 'pass',
        findings: { high: verdict.overturned ? 1 : 0, medium: 0, low: 1 },
        overturned: verdict.overturned ?? false,
        by: verdict.by ?? null,
        disputed: verdict.disputed ?? false,
      },
      evidence: verdict.overturned
        ? [{ label: 'Review notes', path: `/mnt/Cursor/share/example-project/reviews/${local}.md` }]
        : [],
    });
  const flag = (kind, detail, local) => ({
    kind,
    detail,
    evidence: `/mnt/Cursor/share/example-project/evidence/${local}.md`,
    reportedBy: 'scoreboard-reviews',
  });
  const build = (local, task, model, at, fields = {}) =>
    add(LEAD, local, {
      at,
      type: 'worknode-build',
      task,
      size: { tasks: 1 },
      model,
      scope: { filesAssigned: 12, filesRead: 12, recordsAssigned: 20, recordsJudged: 20 },
      ...fields,
    });

  // WorkNode builds. Opus: every item passes; WN-6 took two rounds and counts once.
  for (let i = 1; i <= 5; i += 1) {
    const id = build(`WN-${i}`, `WN-${i}`, OPUS, 100 - i, {
      wall: 1100 + i * 100,
      seq: i === 1 ? 2 : 1,
      cost: {
        credits: 10 + i,
        usd: 1.5,
        quota: { account: 'account-a', window: '5h', deltaPct: 3 },
        tokens: null,
      },
    });
    review(`R-WN-${i}`, id, 90 - i, SOL, { by: 'openai/gpt-6.1-sol high' });
  }
  const round1 = build('WN-6-a', 'WN-6', OPUS, 80, { wall: 1500, cost: { credits: 12 } });
  const round2 = build('WN-6-b', 'WN-6', OPUS, 76, { wall: 700, cost: { credits: 6 } });
  review('R-WN-6-a', round1, 78, SOL, {
    overturned: true,
    verdict: 'fail',
    by: 'openai/gpt-6.1-sol high',
  });
  review('R-WN-6-b', round2, 74, SOL, { by: 'openai/gpt-6.1-sol high' });

  // Sol: three pass, one overturned, one disputed overturn, one cannot-verify, one failed start.
  const sol = [
    { i: 11, verdict: {} },
    { i: 12, verdict: {} },
    { i: 13, verdict: {} },
    {
      i: 14,
      verdict: { overturned: true, verdict: 'fail', by: 'anthropic/claude-opus-5-5 xhigh' },
    },
    {
      i: 15,
      verdict: {
        overturned: true,
        verdict: 'fail',
        disputed: true,
        by: 'anthropic/claude-opus-5-5 xhigh',
      },
    },
    { i: 16, verdict: { verdict: 'cannot-verify' } },
  ];
  for (const { i, verdict } of sol) {
    const id = build(`WN-${i}`, `WN-${i}`, SOL, 70 - i, {
      wall: 800 + i * 10,
      cost: {
        credits: null,
        estimate: {
          source: 'quota-watch',
          method: 'token share at list price',
          credits: 8,
          quota: { account: 'account-b', window: '5h', deltaPct: 2 },
          tokens: null,
        },
      },
    });
    review(`R-WN-${i}`, id, 50 - i, OPUS, verdict);
  }
  build('WN-17', 'WN-17', SOL, 40, { wall: 30, outcome: 'failed', scope: {} });

  // GLM: conduct flags with evidence and most work overturned.
  for (let i = 21; i <= 25; i += 1) {
    const flags =
      i === 21
        ? [
            flag(
              'shortcut',
              'Wrote a script to assign verdicts instead of judging each record',
              'WN-21'
            ),
          ]
        : i === 22
          ? [
              flag(
                'test-weakened',
                'Removed the failing assertion instead of fixing the code',
                'WN-22'
              ),
            ]
          : i === 23
            ? [flag('incomplete-read', 'Read 4 of the 12 files it was assigned', 'WN-23')]
            : [];
    const id = build(`WN-${i}`, `WN-${i}`, GLM, 60 - i, {
      wall: 400,
      flags,
      scope:
        i === 23
          ? { filesAssigned: 12, filesRead: 4, recordsAssigned: 20, recordsJudged: 20 }
          : { filesAssigned: 12, filesRead: 12, recordsAssigned: 20, recordsJudged: 20 },
      cost: {
        credits: 2,
        usd: null,
        quota: { account: 'account-c', window: '5h', deltaPct: 1 },
        tokens: null,
      },
    });
    review(`R-WN-${i}`, id, 30 - i / 10, SOL, {
      overturned: i <= 24,
      verdict: i <= 24 ? 'fail' : 'pass',
    });
  }
  // Below minRuns, and enough runs with no review.
  for (let i = 31; i <= 32; i += 1) build(`WN-${i}`, `WN-${i}`, SONNET, 20 + i / 10);
  for (let i = 41; i <= 45; i += 1) build(`WN-${i}`, `WN-${i}`, HAIKU, 15 + i / 10);

  // The second reviewer on one review, so the review rows reach the bar.
  review('R-of-R-WN-11', `${REVIEWS}--R-WN-11`, 8, SOL, { by: 'openai/gpt-6.1-sol high' });
  review('R-of-R-WN-1', `${REVIEWS}--R-WN-1`, 7.5, OPUS, { by: 'anthropic/claude-opus-5-5 xhigh' });
  // A review naming a run nobody reported: ignored and counted.
  review('R-nobody', 'vm.scoreboard-lead--no-such-run', 7, OPUS);

  // Scout: keyed by process as well as the model.
  const scout = (local, model, process, at, kept, drift, fields = {}) =>
    add(fields.source === 'benchmark' ? BENCH : LEAD, local, {
      at,
      type: 'scout',
      task: local,
      size: { items: 127 },
      wall: 500 + kept,
      model,
      process,
      metrics: { factsKept: kept, factsTotal: 127, verdictsChanged: drift, verdictsTotal: 40 },
      ...fields,
    });
  const v1 = { id: 'scout-pipeline', version: '1.0', sha256: null };
  const v2 = { id: 'scout-pipeline', version: '2.0', sha256: null };
  const bench = { source: 'benchmark', benchmark: 'scout-sv3-t9' };
  for (let i = 1; i <= 5; i += 1) {
    scout(`bench-opus-v2-${i}`, OPUS, v2, 200 - i, 118 + i, 2, bench);
    scout(`bench-sol-v2-${i}`, SOL, v2, 190 - i, 100 + i, 6, bench);
  }
  for (let i = 1; i <= 3; i += 1) scout(`bench-opus-v1-${i}`, OPUS, v1, 210 - i, 70 + i, 12, bench);
  for (let i = 1; i <= 5; i += 1) {
    const id = scout(`scout-opus-v2-${i}`, OPUS, v2, 170 - i, 115 + i, 3);
    if (i === 1) review(`R-scout-${i}`, id, 160, SOL, { by: 'openai/gpt-6.1-sol high' });
  }
  for (let i = 1; i <= 2; i += 1) scout(`scout-sol-${i}`, SOL, null, 150 - i, 90, 5);

  // The planning benchmark: its answer key is not final, so its numbers are preliminary.
  const plan = (local, model, at, silent, fields = {}) =>
    add(BENCH, local, {
      at,
      type: 'plan',
      task: local,
      size: { words: 1500 + at },
      wall: 60 + silent * 5,
      model,
      source: 'benchmark',
      benchmark: 'plan-pilot2',
      process: { id: 'a', version: 'pilot/run_a.py', sha256: null },
      metrics: { silent, goldAgreed: 60, answerKeyFinal: 0, modelSeconds: 50 },
      imported: { from: '/mnt/Cursor/archive/example/pilot/runs.json', at: stamp(now - 6 * HOUR) },
      evidence: [
        {
          label: 'Pilot results (preliminary)',
          path: '/mnt/Cursor/archive/example/pilot/RESULTS.md',
        },
      ],
      ...fields,
    });
  for (let i = 1; i <= 5; i += 1) plan(`plan-sol-${i}`, SOL_MEDIUM, 300 - i, 2 + i);
  for (let i = 1; i <= 6; i += 1) plan(`plan-opus-${i}`, OPUS_HIGH, 290 - i, 1);
  // The same model also has observed planning work: a separate column, never averaged in.
  for (let i = 1; i <= 2; i += 1)
    add(LEAD, `plan-observed-${i}`, {
      at: 12 + i,
      type: 'plan',
      task: `plan-observed-${i}`,
      size: { words: 9000 },
      wall: 2000,
      model: OPUS_HIGH,
    });

  // Imported history: observed runs from earlier evidence.
  for (let i = 1; i <= 2; i += 1)
    add(LEAD, `imported-${i}`, {
      at: 500 + i,
      type: 'build',
      task: `imported-${i}`,
      size: { tasks: 1 },
      model: SONNET,
      imported: {
        from: '/mnt/Cursor/archive/example/closure-reaudit.md',
        at: stamp(now - 5 * HOUR),
      },
    });

  // Mechanical edits with quota as the only measured cost.
  for (let i = 1; i <= 5; i += 1) {
    const id = add(LEAD, `mech-muse-${i}`, {
      at: 11 - i / 10,
      type: 'mechanical',
      task: `mech-muse-${i}`,
      size: { files: 20 },
      wall: 200,
      model: MUSE,
      cost: { credits: null, quota: { account: 'account-d', window: '5h', deltaPct: 4 } },
      scope: { filesAssigned: 20, filesRead: 20 },
    });
    if (i === 1) review(`R-mech-${i}`, id, 9.5, SOL);
  }

  // Runs the catalog does not know, and one with no task type.
  add(LEAD, 'unlisted-model', {
    at: 5,
    type: 'build',
    task: 'unlisted',
    size: { tasks: 1 },
    model: UNLISTED,
  });
  add(LEAD, 'unlisted-type', {
    at: 4,
    type: 'audit-read',
    task: 'B09',
    size: { records: 40 },
    model: GLM,
  });
  add(LEAD, 'no-type', { at: 3, type: null, task: 'unknown-work', size: { tasks: 1 }, model: GLM });
  return runs;
}

function writeRun(root, doc) {
  const dir = path.join(root, 'v1', 'writers', doc.writer, 'runs', doc.endedAt.slice(0, 10));
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, `${doc.id}.json`), `${JSON.stringify(doc, null, 1)}\n`);
  return dir;
}

/** Writes the catalog and runs; returns what it wrote. */
export function writeScoreboardFixture(root, options = {}) {
  const now = options.now ?? Date.now();
  const catalog = path.join(root, 'v1', 'catalog');
  if (fs.existsSync(path.join(catalog, 'models.json')))
    throw new Error(`${root} already has a catalog`);
  fs.mkdirSync(catalog, { recursive: true });
  for (const name of fs.readdirSync(SEEDS))
    fs.copyFileSync(path.join(SEEDS, name), path.join(catalog, name));
  const runs = makeRuns(now);
  for (const doc of runs) writeRun(root, doc);
  // An older copy of WN-1 in an earlier date folder, with a lower seq: the newer one wins.
  const first = runs.find((doc) => doc.id === `${LEAD}--WN-1`);
  const older = stamp(Date.parse(first.endedAt) - 72 * HOUR);
  writeRun(root, { ...first, seq: 1, endedAt: older, writtenAt: older, outcome: 'failed' });
  // One damaged file (NUL bytes from a Mac SMB append) and one temporary file, which is ignored.
  const dir = writeRun(root, runs[runs.length - 1]);
  fs.writeFileSync(path.join(dir, `${LEAD}--damaged.json`), Buffer.from('{"schema":\0\0\0'));
  fs.writeFileSync(path.join(dir, `.${LEAD}--half-written.json.tmp-1`), '{"schema"');
  return { root, runs: runs.map((doc) => doc.id), damaged: `${LEAD}--damaged.json` };
}

function main(argv) {
  const args = argv.slice(2);
  if (args.length !== 1) {
    process.stderr.write('Usage: node scripts/scoreboard-fixture.mjs <reports-dir>\n');
    return 2;
  }
  const result = writeScoreboardFixture(path.resolve(args[0]));
  process.stdout.write(
    `Wrote ${result.runs.length} runs and the catalog to ${result.root}.\nServer environment:\n  AAC_AGENT_REPORTS_DIR=${result.root}\n`
  );
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href)
  process.exitCode = main(process.argv);
