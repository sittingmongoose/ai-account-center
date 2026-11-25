#!/usr/bin/env node
/**
 * Deterministic real-sized synthetic fixture generator for the Phase 4 dashboard
 * memory budget test. Generates zero real data: neutral synthetic identifiers,
 * @example.invalid emails, and seeded PRNG values only.
 *
 * Layout written under <outDir>:
 *   - agent-reports/v1/catalog/            packaged Scoreboard catalog seeds
 *   - agent-reports/v1/writers/<writer>/   ~2,000 writers with thread/status/tasks
 *   - agent-reports/v1/writers/<w>/runs/   ~7,500 run files from ~20 writers over 60 days
 *   - agent-reports/pm-mail-registry/      ~4,500 mail registry entries
 *   - usage-logs/                          3 providers x 19 accounts x 60 days (quota + activity)
 *   - home/.ccs/models-dev-registry-cache.json  fresh synthetic pricing cache (no live HTTP)
 *   - home/.ccs/status/                    deterministic signed answer history
 *   - home/.ccs/store/                     native redb store migrated from logs and runs
 *   - fixture-manifest.json                deterministic generation summary
 *
 * Usage:
 *   node scripts/perf/memory-fixture.mjs <out-dir> [--seed 20261125] [--now <epochMs>]
 *     [--runs 7500] [--writers 2000] [--registry 4500] [--days 60] [--accounts 19]
 *     [--skip-store] [--store-module <path>]
 */
import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { writeScoreboardFixture } from '../scoreboard-fixture.mjs';
import { writeStatusFixture } from '../status-fixture.mjs';
import { STATIC_RATES } from '../../web-dashboard/public/model-rates.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..', '..');
const CATALOG_DIR = path.join(REPO_ROOT, 'docs', 'agent-reports', 'catalog');
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

export const DEFAULT_OPTIONS = Object.freeze({
  seed: 20261125,
  runs: 7500,
  runWriters: 20,
  writers: 2000,
  registryEntries: 4500,
  days: 60,
  accounts: 19,
  quotaSamplesPerDay: 24,
  activitySessions: 1800,
  pricingProviders: 200,
  pricingModelsPerProvider: 40,
  skipStore: false,
  storeModule: null,
  now: null,
});

const MACHINES = ['vm', 'mac', 'windows', 'nas1'];
const HOSTS = ['ubuntu', 'mac', 'windows', 'nas1'];
const PROJECTS = ['example-alpha', 'example-beta', 'example-gamma', 'example-delta'];
const STREAMS = ['CORE', 'UI', 'DATA', 'OPS', 'TESTS', 'DOCS', 'PERF', 'SYNC'];
const THREAD_STATES = ['working', 'waiting', 'blocked', 'paused', 'idle', 'done'];
const TASK_STATES = ['todo', 'ready', 'working', 'review', 'fixing', 'blocked', 'done', 'landed'];
const FLAG_KINDS = [
  'shortcut',
  'incomplete-read',
  'test-weakened',
  'scope-cut',
  'false-claim',
];

const PROVIDER_SPECS = [
  {
    id: 'claude',
    tool: 'claude',
    route: 'anthropic',
    platform: 'mac',
    source: 'Claude quota on Mac',
    plan: 'max',
    share: 7,
    models: [
      { id: 'claude-opus-4-7', weight: 0.4, cacheK: 8.5, cacheCreate: 0.32 },
      { id: 'claude-opus-4-6', weight: 0.15, cacheK: 7.5, cacheCreate: 0.3 },
      { id: 'claude-sonnet-4-6', weight: 0.25, cacheK: 6.5, cacheCreate: 0.25 },
      { id: 'claude-sonnet-4-5', weight: 0.1, cacheK: 5.5, cacheCreate: 0.22 },
      { id: 'claude-haiku-4-5', weight: 0.1, cacheK: 4.0, cacheCreate: 0.18 },
    ],
  },
  {
    id: 'codex',
    tool: 'codex',
    route: 'openai',
    platform: 'ubuntu',
    source: 'Codex quota on Ubuntu',
    plan: 'pro',
    share: 6,
    models: [
      { id: 'gpt-5-codex', weight: 0.45, cacheK: 5.5, cacheCreate: 0 },
      { id: 'gpt-5.2-codex', weight: 0.25, cacheK: 5.0, cacheCreate: 0 },
      { id: 'gpt-5', weight: 0.2, cacheK: 4.0, cacheCreate: 0 },
      { id: 'gpt-5-mini', weight: 0.1, cacheK: 3.0, cacheCreate: 0 },
    ],
  },
  {
    id: 'antigravity',
    tool: 'antigravity',
    route: 'google-antigravity',
    platform: 'ubuntu',
    source: 'Antigravity quota on Ubuntu',
    plan: 'pro',
    share: 6,
    models: [
      { id: 'gemini-2.5-pro', weight: 0.45, cacheK: 3.5, cacheCreate: 0 },
      { id: 'gemini-2.5-flash', weight: 0.3, cacheK: 2.5, cacheCreate: 0 },
      { id: 'gemini-3-pro-preview', weight: 0.15, cacheK: 3.0, cacheCreate: 0 },
      { id: 'gemini-3-flash-preview', weight: 0.1, cacheK: 2.0, cacheCreate: 0 },
    ],
  },
];

/** Deterministic Mulberry32 PRNG. */
export function createRng(seed) {
  let state = (Number(seed) >>> 0) || 1;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return {
    next,
    int(min, max) {
      return min + Math.floor(next() * (max - min + 1));
    },
    pick(list) {
      return list[Math.floor(next() * list.length)];
    },
    hex(len) {
      let out = '';
      while (out.length < len) {
        out += Math.floor(next() * 0x100000000)
          .toString(16)
          .padStart(8, '0');
      }
      return out.slice(0, len);
    },
  };
}

function sha256Hex(value) {
  return createHash('sha256').update(value).digest('hex');
}

function isoSec(ms) {
  return new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z');
}

function isoMs(ms) {
  return new Date(ms).toISOString();
}

function pad(n, width) {
  return String(n).padStart(width, '0');
}

function canonicalJson(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const keys = Object.keys(value)
    .filter((key) => value[key] !== undefined)
    .sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
}

/** RFC 8410 deterministic Ed25519 PKCS#8 private key from a seed. */
function deterministicEd25519Key(seed) {
  const rawSeed = createHash('sha256').update(`aac-memory-fixture-ed25519:${seed}`).digest();
  const pkcs8Prefix = Buffer.from('302e020100300506032b657004220420', 'hex');
  const privateKey = createPrivateKey({
    key: Buffer.concat([pkcs8Prefix, rawSeed]),
    format: 'der',
    type: 'pkcs8',
  });
  const publicKey = createPublicKey(privateKey);
  const keyId = createHash('sha256')
    .update(publicKey.export({ type: 'spki', format: 'der' }))
    .digest('hex')
    .slice(0, 16);
  return {
    privateKey,
    privateKeyPem: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    publicKeyPem: publicKey.export({ type: 'spki', format: 'pem' }).toString(),
    keyId,
  };
}

function writeJsonCompact(file, value, mtimeMs = null) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const body = `${JSON.stringify(value)}\n`;
  fs.writeFileSync(file, body);
  if (mtimeMs !== null) {
    const dt = new Date(mtimeMs);
    fs.utimesSync(file, dt, dt);
  }
  return Buffer.from(body);
}

/** Load catalog models and task types so synthetic runs align with packaged seeds. */
function loadCatalogSeeds() {
  const modelsDoc = JSON.parse(fs.readFileSync(path.join(CATALOG_DIR, 'models.json'), 'utf8'));
  const typesDoc = JSON.parse(fs.readFileSync(path.join(CATALOG_DIR, 'task-types.json'), 'utf8'));
  const models = (modelsDoc.models ?? []).map((entry) => {
    const slash = entry.model.indexOf('/');
    const provider = slash > 0 ? entry.model.slice(0, slash) : 'example';
    const id = slash > 0 ? entry.model.slice(slash + 1) : entry.model;
    return {
      full: entry.model,
      provider,
      id,
      harness: entry.runtimes?.[0] ?? 'omp',
      efforts: entry.efforts?.length ? entry.efforts : [null],
    };
  });
  const taskTypes = (typesDoc.taskTypes ?? []).map((entry) => entry.id);
  return { models, taskTypes };
}

/** Deterministically sign the base status-fixture answers (without random UUID/keygen). */
function seedDeterministicStatusAnswers(reportsDir, ccsDir, seed, now) {
  const v1 = path.join(reportsDir, 'v1');
  const writersDir = path.join(v1, 'writers');
  const storeDir = path.join(ccsDir, 'status');
  fs.mkdirSync(storeDir, { recursive: true, mode: 0o700 });
  const key = deterministicEd25519Key(seed);
  fs.writeFileSync(path.join(storeDir, 'answer-key.pem'), key.privateKeyPem, { mode: 0o600 });
  fs.mkdirSync(path.join(v1, 'keys'), { recursive: true });
  fs.writeFileSync(path.join(v1, 'keys', 'aac-answers.pub'), key.publicKeyPem);
  fs.writeFileSync(path.join(v1, 'keys', `${key.keyId}.pub`), key.publicKeyPem);

  const targets = [
    {
      writer: 'mac.aac-claude-relogin',
      cardId: 'mac.aac-claude-relogin--Q7',
      choice: ['B'],
      note: 'Windows gets its own runner; keep the Mac one simple.',
      via: 'deck',
      ago: 20 * HOUR,
    },
    {
      writer: 'vm.pm-closure-lead',
      cardId: 'vm.pm-closure-lead--Q1',
      choice: ['A'],
      note: '',
      via: 'recommended',
      ago: 3 * HOUR,
    },
    {
      writer: 'vm.aac-status-lead',
      cardId: 'vm.aac-status-lead--Q2',
      choice: [],
      note: 'Could you show both side by side first?',
      via: 'deck',
      ago: 30 * MIN,
    },
  ];

  const lines = [];
  for (let i = 0; i < targets.length; i += 1) {
    const entry = targets[i];
    const cardPath = path.join(writersDir, entry.writer, 'cards', `${entry.cardId}.json`);
    if (!fs.existsSync(cardPath)) continue;
    const raw = fs.readFileSync(cardPath);
    const doc = JSON.parse(raw.toString('utf8'));
    const cardSha = sha256Hex(raw);
    const unsigned = {
      schema: 'agent-reports/answer@1',
      cardId: entry.cardId,
      cardSeq: doc.seq,
      cardSha256: cardSha,
      kind: entry.choice.length > 0 ? 'answer' : 'note',
      choice: entry.choice,
      note: entry.note,
      via: entry.via,
      answeredAt: isoMs(now - entry.ago),
      answeredBy: 'owner',
      device: 'iPhone',
    };
    const signature = {
      alg: 'ed25519',
      keyId: key.keyId,
      value: sign(null, Buffer.from(canonicalJson(unsigned), 'utf8'), key.privateKey).toString(
        'base64'
      ),
    };
    const answer = { ...unsigned, signature };
    const idHash = sha256Hex(`answer-uuid:${seed}:${i}:${entry.cardId}`);
    const id = `${idHash.slice(0, 8)}-${idHash.slice(8, 12)}-4${idHash.slice(13, 16)}-8${idHash.slice(17, 20)}-${idHash.slice(20, 32)}`;
    const file =
      answer.kind === 'note'
        ? `${entry.cardId}.note-${id.replace(/-/g, '').slice(0, 12)}.json`
        : `${entry.cardId}.json`;
    writeJsonCompact(path.join(v1, 'answers', file), answer);
    const deliveredAt = isoMs(now - entry.ago + MIN);
    lines.push(
      { type: 'answer', id, at: unsigned.answeredAt, answer, card: doc },
      { type: 'delivery', id, step: 'share', state: 'done', at: deliveredAt, detail: 'Signed copy written to the share.' },
      { type: 'delivery', id, step: 'doorbell', state: 'done', at: deliveredAt, detail: `Rang ${doc.replyTo.pmMail} through pm-mail.` }
    );
  }
  fs.writeFileSync(
    path.join(storeDir, 'answers.jsonl'),
    lines.map((line) => `${JSON.stringify(line)}\n`).join(''),
    { mode: 0o600 }
  );
}

/** Build the 19 synthetic accounts distributed across the 3 providers. */
export function buildSyntheticAccounts(totalAccounts = 19) {
  const accounts = [];
  const totalShare = PROVIDER_SPECS.reduce((acc, spec) => acc + spec.share, 0);
  const allocations = PROVIDER_SPECS.map((spec, idx) => {
    if (idx === PROVIDER_SPECS.length - 1) return 0;
    return Math.max(1, Math.round((totalAccounts * spec.share) / totalShare));
  });
  const allocated = allocations.reduce((a, b) => a + b, 0);
  allocations[PROVIDER_SPECS.length - 1] = Math.max(1, totalAccounts - allocated);
  while (allocations.reduce((a, b) => a + b, 0) > totalAccounts) {
    const maxIdx = allocations.indexOf(Math.max(...allocations));
    allocations[maxIdx] -= 1;
  }

  for (let pIdx = 0; pIdx < PROVIDER_SPECS.length; pIdx += 1) {
    const spec = PROVIDER_SPECS[pIdx];
    const count = allocations[pIdx];
    for (let i = 1; i <= count; i += 1) {
      const id = `${spec.id}:synth-${pad(i, 2)}`;
      const email = `synth.${spec.id}.${pad(i, 2)}@example.invalid`;
      const identity = sha256Hex(JSON.stringify([spec.id, id, email.toLowerCase()]));
      accounts.push({
        index: accounts.length,
        id,
        provider: spec.id,
        tool: spec.tool,
        route: spec.route,
        email,
        identity,
        plan: spec.plan,
        platform: spec.platform,
        source: spec.source,
        isActive: i === 1,
        models: spec.models,
      });
    }
  }
  return accounts;
}

/** Write ~2,000 writer folders (thread.json, status.json, tasks.json) + return the ~20 run writers. */
function generateWriters(reportsDir, rng, now, totalWriters, runWriterCount) {
  const writersDir = path.join(reportsDir, 'v1', 'writers');
  const existing = fs.existsSync(writersDir)
    ? fs.readdirSync(writersDir).filter((name) => fs.statSync(path.join(writersDir, name)).isDirectory())
    : [];
  const remaining = Math.max(0, totalWriters - existing.length);
  const runWriters = [];
  const allCreated = [];
  const catalog = loadCatalogSeeds();

  for (let i = 0; i < remaining; i += 1) {
    const isRunWriter = i < runWriterCount;
    const machine = MACHINES[i % MACHINES.length];
    const shortName = isRunWriter ? `synth-run-${pad(i + 1, 2)}` : `synth-w-${pad(i + 1, 4)}`;
    const writer = `${machine}.${shortName}`;
    const writerDir = path.join(writersDir, writer);
    fs.mkdirSync(writerDir, { recursive: true });
    if (isRunWriter) runWriters.push(writer);
    allCreated.push(writer);

    // Spread ages: newest ~900 writers within the last 48h, the rest spread across 2..60 days.
    const isRecent = isRunWriter || i < Math.floor(remaining * 0.48);
    const agoMs = isRecent
      ? rng.int(2, 48 * 60) * MIN
      : rng.int(49, 60 * 24) * HOUR + rng.int(0, 59) * MIN;
    const writtenMs = now - agoMs;
    const writtenAt = isoSec(writtenMs);
    const project = PROJECTS[i % PROJECTS.length];
    const stream = STREAMS[i % STREAMS.length];
    const state = isRecent ? THREAD_STATES[i % 4] : i % 3 === 0 ? 'idle' : 'done';
    const model = catalog.models[i % catalog.models.length];
    const seq = 10 + (i % 40);
    const t3Thread = `synth-thread-${pad(i + 1, 5)}`;

    const threadDoc = {
      schema: 'agent-reports/thread@1',
      writer,
      seq,
      writtenAt,
      ttlSeconds: state === 'done' || state === 'idle' ? null : 1800,
      agent: {
        name: shortName,
        harness: model.harness,
        model: model.id,
        effort: model.efforts[0],
        account: `synth-acct-${pad((i % 19) + 1, 2)}`,
        machine,
        t3Thread,
        parent: null,
      },
      project,
      stream,
      state,
      doing: `Synthetic workload item ${shortName} on ${stream}`,
      blockedOn:
        state === 'blocked'
          ? {
              kind: 'review',
              what: `Waiting on synthetic review for ${shortName}`,
              since: isoSec(writtenMs - 20 * MIN),
            }
          : null,
      progress: { done: i % 8, total: 8, unit: 'items' },
      links: [],
    };
    writeJsonCompact(path.join(writerDir, 'thread.json'), threadDoc, writtenMs);

    const statusDoc = {
      schema: 'agent-reports/status@1',
      writer,
      seq,
      writtenAt,
      t3Thread,
      summary: `Synthetic status summary for ${shortName} in stream ${stream}.`,
      percent: Math.min(100, (i % 11) * 10),
      done: [
        { id: `D-${i}-1`, title: `Completed baseline task for ${shortName}` },
        { id: `D-${i}-2`, title: `Verified synthetic check for ${shortName}` },
      ],
      left:
        state === 'done'
          ? []
          : [
              { id: `L-${i}-1`, title: `Finish synthetic step A for ${shortName}` },
              { id: `L-${i}-2`, title: `Review synthetic step B for ${shortName}` },
            ],
      blockers:
        state === 'blocked'
          ? [{ text: `Synthetic review pending for ${shortName}`, waitingOn: 'review' }]
          : [],
      looping: { state: 'no', text: null },
    };
    writeJsonCompact(path.join(writerDir, 'status.json'), statusDoc, writtenMs);

    const itemCount = isRunWriter ? 6 : 2 + (i % 3);
    const items = [];
    for (let k = 0; k < itemCount; k += 1) {
      const itemState = TASK_STATES[(i + k) % TASK_STATES.length];
      items.push({
        id: `T-${pad(i + 1, 4)}-${k + 1}`,
        title: `Synthetic task ${k + 1} for ${shortName}`,
        state: itemState,
        owner: shortName,
        branch: null,
        head: null,
        reviews: [],
        landed: itemState === 'landed' ? { sha: rng.hex(10), at: writtenAt } : null,
        blockedOn: null,
        why: null,
        updatedAt: writtenAt,
      });
    }
    const tasksDoc = {
      schema: 'agent-reports/tasks@1',
      writer,
      seq,
      writtenAt,
      project,
      projectLabel: `Project ${project}`,
      stream,
      streamLabel: `Stream ${stream}`,
      owner: shortName,
      goal: `Synthetic stream goal for ${stream}`,
      board: null,
      items,
    };
    writeJsonCompact(path.join(writerDir, 'tasks.json'), tasksDoc, writtenMs);
    const dt = new Date(writtenMs);
    fs.utimesSync(writerDir, dt, dt);
  }

  return {
    totalWriters: existing.length + allCreated.length,
    createdWriters: allCreated.length,
    runWriters: runWriters.length ? runWriters : existing.slice(0, Math.max(1, runWriterCount)),
  };
}

/** Write ~7,500 run files spread across ~20 writers and 60 days. */
function generateRuns(reportsDir, runWriters, rng, now, totalRuns, days, existingRunCount) {
  const toCreate = Math.max(0, totalRuns - existingRunCount);
  const { models, taskTypes } = loadCatalogSeeds();
  const workTypes = taskTypes.filter((t) => t !== 'review');
  const writersDir = path.join(reportsDir, 'v1', 'writers');
  const createdWorkIds = [];
  const ensuredDirs = new Set();

  for (let i = 0; i < toCreate; i += 1) {
    const writer = runWriters[i % runWriters.length];
    const isReview = createdWorkIds.length > 0 && i % 4 === 3;
    const ageMs = Math.floor(((toCreate - 1 - i) / Math.max(1, toCreate)) * (days * DAY - 2 * HOUR)) + HOUR;
    const jitterMs = rng.int(0, 45) * MIN;
    const endedMs = Math.min(now - 5 * MIN, now - ageMs + jitterMs);
    const wall = isReview ? rng.int(120, 600) : rng.int(240, 2400);
    const startedMs = endedMs - wall * 1000;
    const model = models[(i * 7 + rng.int(0, models.length - 1)) % models.length];
    const effort = rng.pick(model.efforts);
    const taskType = isReview ? 'review' : workTypes[i % workTypes.length];
    const localId = `${isReview ? 'REV' : 'RUN'}-${pad(i + 1, 5)}`;
    const id = `${writer}--${localId}`;
    const dateDir = isoSec(endedMs).slice(0, 10);
    const dir = path.join(writersDir, writer, 'runs', dateDir);
    if (!ensuredDirs.has(dir)) {
      fs.mkdirSync(dir, { recursive: true });
      ensuredDirs.add(dir);
    }

    const isBench = !isReview && (taskType === 'plan' || taskType === 'scout') && i % 5 === 0;
    const outcomeRoll = rng.next();
    const outcome = outcomeRoll < 0.92 ? 'done' : outcomeRoll < 0.97 ? 'failed' : 'partial';
    const hasFlag = !isReview && outcome === 'done' && rng.next() < 0.03;
    const doc = {
      schema: 'agent-reports/run@1',
      id,
      writer,
      seq: i % 19 === 0 ? 2 : 1,
      writtenAt: isoSec(endedMs + 30_000),
      source: isBench ? 'benchmark' : 'observed',
      ...(isBench ? { benchmark: taskType === 'plan' ? 'plan-pilot2' : 'scout-sv3-t9' } : {}),
      project: PROJECTS[i % PROJECTS.length],
      stream: STREAMS[i % STREAMS.length],
      task: {
        id: `SYNTH-${pad(i + 1, 5)}`,
        type: taskType,
        title: `Synthetic ${taskType} ${localId}`,
        size:
          taskType === 'review' || taskType === 'audit-read'
            ? { records: rng.int(10, 80) }
            : taskType === 'mechanical' || taskType === 'docs'
              ? { files: rng.int(2, 25) }
              : taskType === 'plan'
                ? { words: rng.int(1200, 9000) }
                : taskType === 'scout'
                  ? { items: rng.int(30, 150) }
                  : { tasks: 1 },
      },
      model: {
        provider: model.provider,
        id: model.id,
        harness: model.harness,
        effort,
        account: `synth-acct-${pad((i % 19) + 1, 2)}`,
      },
      startedAt: isoSec(startedMs),
      endedAt: isoSec(endedMs),
      wallSeconds: wall,
      outcome,
      cost: {
        credits: +(1 + rng.next() * 14).toFixed(2),
        usd: +(0.25 + rng.next() * 4.5).toFixed(2),
        quota: {
          account: `synth-acct-${pad((i % 19) + 1, 2)}`,
          window: '5h',
          deltaPct: +(0.5 + rng.next() * 5).toFixed(1),
        },
        tokens: {
          input: rng.int(10_000, 400_000),
          output: rng.int(1_500, 35_000),
        },
      },
      scope: {
        filesAssigned: 12,
        filesRead: hasFlag ? 5 : 12,
        recordsAssigned: 20,
        recordsJudged: 20,
      },
      flags: hasFlag
        ? [
            {
              kind: rng.pick(FLAG_KINDS),
              detail: `Synthetic conduct finding on ${localId}`,
              evidence: `/mnt/Cursor/share/example-project/evidence/${localId}.md`,
              reportedBy: 'synth-reviewer',
            },
          ]
        : [],
      evidence:
        i % 6 === 0
          ? [{ label: 'Synthetic report', path: `/mnt/Cursor/share/example-project/runs/${localId}.md` }]
          : [],
    };

    if (taskType === 'scout') {
      doc.process = { id: 'scout-pipeline', version: i % 2 === 0 ? '2.0' : '1.0', sha256: null };
      doc.metrics = {
        factsKept: rng.int(80, 125),
        factsTotal: 127,
        verdictsChanged: rng.int(0, 10),
        verdictsTotal: 40,
      };
    } else if (taskType === 'plan') {
      doc.process = { id: 'a', version: 'pilot/run_a.py', sha256: null };
      doc.metrics = {
        silent: rng.int(0, 8),
        goldAgreed: rng.int(45, 65),
        answerKeyFinal: 1,
        modelSeconds: rng.int(30, 180),
      };
    }

    if (isReview) {
      const targetId = createdWorkIds[Math.max(0, createdWorkIds.length - rng.int(1, Math.min(20, createdWorkIds.length)))];
      const verdictRoll = rng.next();
      const verdict = verdictRoll < 0.78 ? 'pass' : verdictRoll < 0.96 ? 'fail' : 'cannot-verify';
      const overturned = verdict === 'fail';
      doc.review = {
        of: targetId,
        verdict,
        findings: {
          high: overturned ? rng.int(1, 2) : 0,
          medium: rng.int(0, 2),
          low: rng.int(0, 3),
        },
        overturned,
        by: `${model.full}${effort ? ` ${effort}` : ''}`,
        disputed: overturned && rng.next() < 0.15,
      };
    } else {
      createdWorkIds.push(id);
    }

    writeJsonCompact(path.join(dir, `${id}.json`), doc, endedMs);
  }
  return { totalRuns: existingRunCount + toCreate, createdRuns: toCreate };
}

/** Write ~4,500 pm-mail registry entries. */
function generateMailRegistry(reportsDir, rng, now, totalEntries) {
  const regDir = path.join(reportsDir, 'pm-mail-registry');
  fs.mkdirSync(regDir, { recursive: true });
  const existing = fs.readdirSync(regDir).filter((name) => name.endsWith('.json')).length;
  const toCreate = Math.max(0, totalEntries - existing);
  const kinds = ['claude', 'codex', 'omp', 'muse', 'zcode'];
  const hostMap = {
    vm: { machine: 'ubuntuserver', cwd: '/home/agent/work/example-project' },
    mac: { machine: 'agent-mac.local', cwd: '/Users/agent/work/example-project' },
    windows: { machine: 'win-workstation', cwd: 'C:\\Users\\agent\\work\\example-project' },
    nas1: { machine: 'nas1-host', cwd: '/data/agent/work/example-project' },
  };

  for (let i = 0; i < toCreate; i += 1) {
    const name = `synth-mail-${pad(i + 1, 5)}`;
    const machineKey = MACHINES[i % MACHINES.length];
    const host = hostMap[machineKey];
    // Realistic recency mix: ~250 seen in last 24h, ~800 in last 7d, rest 7..60d.
    const agoMs =
      i < 250
        ? rng.int(1, 24 * 60) * MIN
        : i < 1050
          ? rng.int(25, 7 * 24) * HOUR
          : rng.int(8 * 24, 60 * 24) * HOUR;
    const lastSeen = now - agoMs;
    const entry = {
      name,
      kind: kinds[i % kinds.length],
      cwd: host.cwd,
      about: `Synthetic thread ${name} handling ${STREAMS[i % STREAMS.length]} stream work`,
      registered: isoMs(lastSeen - 2 * HOUR),
      last_seen: isoMs(lastSeen),
      machine: host.machine,
    };
    writeJsonCompact(path.join(regDir, `${name}.json`), entry, lastSeen);
  }
  return { totalRegistryEntries: existing + toCreate, createdRegistryEntries: toCreate };
}

/** Write a fresh deterministic synthetic models-dev-registry-cache.json (zero live HTTP). */
function generateModelsDevCache(ccsDir, rng, now, providerCount, modelsPerProvider) {
  const providers = {};
  // Seed real static-rate models under their canonical providers so pricing resolves cleanly.
  const seeded = {
    anthropic: [
      'claude-opus-4-7',
      'claude-opus-4-6',
      'claude-sonnet-4-6',
      'claude-sonnet-4-5',
      'claude-haiku-4-5',
    ],
    openai: ['gpt-5-codex', 'gpt-5.2-codex', 'gpt-5', 'gpt-5-mini'],
    google: ['gemini-2.5-pro', 'gemini-2.5-flash', 'gemini-3-pro-preview', 'gemini-3-flash-preview'],
  };
  for (const [providerId, modelIds] of Object.entries(seeded)) {
    const models = {};
    for (const modelId of modelIds) {
      const rate = STATIC_RATES[modelId] ?? [3, 15, 3.75, 0.3];
      models[modelId] = {
        id: modelId,
        name: `Synthetic ${modelId}`,
        family: providerId,
        attachment: true,
        reasoning: true,
        tool_call: true,
        limit: { context: 200000, output: 32000 },
        cost: {
          input: rate[0],
          output: rate[1],
          cache_write: rate[2],
          cache_read: rate[3],
        },
      };
    }
    providers[providerId] = {
      id: providerId,
      name: `Synthetic ${providerId}`,
      models,
    };
  }
  // Pad to a realistic catalog size (~8,000 models) so models_dev pruning & indexing is exercised.
  for (let p = 0; p < providerCount; p += 1) {
    const providerId = `synth-provider-${pad(p + 1, 3)}`;
    const models = {};
    for (let m = 0; m < modelsPerProvider; m += 1) {
      const modelId = `${providerId}-model-${pad(m + 1, 2)}`;
      const input = +(0.25 + rng.next() * 14).toFixed(2);
      const output = +(input * (3 + rng.next() * 2)).toFixed(2);
      models[modelId] = {
        id: modelId,
        name: `Synthetic Model ${p + 1}.${m + 1}`,
        family: providerId,
        attachment: m % 2 === 0,
        reasoning: m % 3 === 0,
        limit: { context: 128000, output: 16384 },
        modalities: { input: ['text'], output: ['text'] },
        release_date: '2026-06-01',
        cost: {
          input,
          output,
          cache_read: +(input * 0.1).toFixed(3),
          cache_write: +(input * 1.25).toFixed(3),
        },
      };
    }
    providers[providerId] = {
      id: providerId,
      name: `Synthetic Provider ${p + 1}`,
      npm: `@example/${providerId}`,
      models,
    };
  }
  const cacheFile = path.join(ccsDir, 'models-dev-registry-cache.json');
  const payload = { version: 1, fetchedAt: now, providers };
  const bytes = writeJsonCompact(cacheFile, payload, now);
  return {
    file: cacheFile,
    providers: Object.keys(providers).length,
    models: Object.values(providers).reduce((n, p) => n + Object.keys(p.models).length, 0),
    bytes: bytes.length,
  };
}

function windowIdentity(win) {
  const parts = [win.key, win.kind ?? null, win.unit ?? null];
  return JSON.stringify(parts);
}

/** Build quota windows for one synthetic account at timestamp ms. */
function syntheticAccountWindows(account, tMs, startMs, rng) {
  const minute = Math.floor((tMs - startMs) / MIN);
  const i = account.index;
  const hourOfDay = new Date(tMs).getUTCHours();
  const busy = hourOfDay >= 8 + (i % 5) && hourOfDay < 17 + (i % 4);
  const fiveStart = Math.floor(minute / 300) * 300;
  const weekStart = Math.floor(minute / 10080) * 10080;
  const five = +(
    Math.min(
      100,
      (busy ? (minute - fiveStart) / (3.2 + (i % 4)) : ((fiveStart / 300) % 9) * 2.5) + rng.next() * 1.5
    )
  ).toFixed(1);
  const week = +(Math.min(100, (minute - weekStart) / (95 + i * 4) + rng.next() * 0.8)).toFixed(1);
  const fiveReset = isoMs(startMs + (fiveStart + 300) * MIN);
  const weekReset = isoMs(startMs + (weekStart + 10080) * MIN);

  if (account.provider === 'claude') {
    const opus = +(Math.min(100, week * 0.55)).toFixed(1);
    return [
      {
        key: 'five_hour',
        label: 'Five-hour usage',
        kind: 'rate_limit',
        usedPercent: five,
        remainingPercent: +(100 - five).toFixed(1),
        resetAt: fiveReset,
        windowMinutes: 300,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
      },
      {
        key: 'seven_day',
        label: 'Weekly usage',
        kind: 'rate_limit',
        usedPercent: week,
        remainingPercent: +(100 - week).toFixed(1),
        resetAt: weekReset,
        windowMinutes: 10080,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
      },
      {
        key: 'seven_day_opus',
        label: 'Weekly Opus',
        kind: 'rate_limit',
        usedPercent: opus,
        remainingPercent: +(100 - opus).toFixed(1),
        resetAt: weekReset,
        windowMinutes: 10080,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
      },
    ];
  }
  if (account.provider === 'codex') {
    const credits = Math.max(0, Math.round(5000 - minute / (35 + i * 2)));
    return [
      {
        key: 'five_hour',
        label: '5h',
        kind: 'rate_limit',
        usedPercent: five,
        remainingPercent: +(100 - five).toFixed(1),
        resetAt: fiveReset,
        windowMinutes: 300,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
      },
      {
        key: 'seven_day',
        label: 'week',
        kind: 'rate_limit',
        usedPercent: week,
        remainingPercent: +(100 - week).toFixed(1),
        resetAt: weekReset,
        windowMinutes: 10080,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
      },
      {
        key: 'credits',
        label: 'Credits',
        kind: 'balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        remaining: credits,
        unit: 'credits',
      },
    ];
  }
  const flash = +(Math.min(100, five * 0.4)).toFixed(1);
  return [
    {
      key: 'gemini-pro',
      label: 'Gemini Pro',
      kind: 'rate_limit',
      usedPercent: five,
      remainingPercent: +(100 - five).toFixed(1),
      resetAt: fiveReset,
      windowMinutes: 300,
      used: null,
      limit: null,
      remaining: null,
      unit: null,
    },
    {
      key: 'gemini-flash',
      label: 'Gemini Flash',
      kind: 'rate_limit',
      usedPercent: flash,
      remainingPercent: +(100 - flash).toFixed(1),
      resetAt: fiveReset,
      windowMinutes: 300,
      used: null,
      limit: null,
      remaining: null,
      unit: null,
    },
  ];
}

function modelCostUsd(modelId, tokens) {
  const rate = STATIC_RATES[modelId] ?? [3, 15, 3.75, 0.3];
  return +(
    (tokens.inputTokens * rate[0] +
      tokens.outputTokens * rate[1] +
      tokens.cacheCreationTokens * rate[2] +
      tokens.cacheReadTokens * rate[3]) /
    1e6
  ).toFixed(6);
}

/**
 * Generate Analytics usage logs on disk for 3 providers x 19 accounts x 60 days:
 *   - usage-logs/quota-history-day-XX.json   (quota observations per day)
 *   - usage-logs/activity-<provider>.jsonl   (hourly token/cost/session usage logs)
 */
function generateAnalyticsUsageLogs(usageLogsDir, accounts, rng, now, days, quotaSamplesPerDay, activitySessions) {
  fs.mkdirSync(usageLogsDir, { recursive: true });
  const quotaDir = path.join(usageLogsDir, 'quota-history');
  fs.mkdirSync(quotaDir, { recursive: true });

  const startMs = now - days * DAY;
  const stepMs = Math.max(5 * MIN, Math.floor(DAY / Math.max(1, quotaSamplesPerDay)));
  let quotaObservations = 0;
  const quotaFiles = [];

  for (let d = 0; d < days; d += 1) {
    const dayStart = startMs + d * DAY;
    const records = [];
    for (let s = 0; s < quotaSamplesPerDay; s += 1) {
      const t = dayStart + s * stepMs;
      if (t >= now) break;
      const stamped = isoMs(t);
      for (const account of accounts) {
        const status = rng.next() < 0.004 ? 'error' : 'ok';
        records.push({
          identity: account.identity,
          provider: account.provider,
          accountId: account.id,
          plan: account.plan,
          sampledAt: stamped,
          observedAt: stamped,
          platform: account.platform,
          source: account.source,
          status,
          isActive: account.isActive,
          windows: status === 'ok' ? syntheticAccountWindows(account, t, startMs, rng) : [],
        });
      }
    }
    quotaObservations += records.length;
    const dayFile = path.join(quotaDir, `quota-day-${pad(d + 1, 3)}.json`);
    writeJsonCompact(dayFile, {
      schemaVersion: 1,
      collectedSince: isoMs(startMs),
      records,
    });
    quotaFiles.push(dayFile);
  }

  // Generate hourly token/cost activity usage logs for the 3 providers across the 19 accounts and 4 hosts.
  const accountsByProvider = new Map();
  for (const account of accounts) {
    if (!accountsByProvider.has(account.provider)) accountsByProvider.set(account.provider, []);
    accountsByProvider.get(account.provider).push(account);
  }

  // Build deterministic sessions distributed across providers, accounts, hosts, and the 60-day span.
  const sessionsPerProvider = new Map();
  for (const spec of PROVIDER_SPECS) sessionsPerProvider.set(spec.id, []);
  for (let s = 0; s < activitySessions; s += 1) {
    const account = accounts[s % accounts.length];
    const host = HOSTS[(s * 3 + account.index) % HOSTS.length];
    const sessionHash = sha256Hex(`aac-synth-session:${account.provider}:${s}`);
    const sessionKey = sessionHash.slice(0, 16);
    const spanHours = rng.int(2, 18);
    const startHourOffset = rng.int(0, Math.max(0, days * 24 - spanHours - 1));
    const firstMs = startMs + startHourOffset * HOUR;
    const lastMs = Math.min(now - HOUR, firstMs + spanHours * HOUR);
    sessionsPerProvider.get(account.provider).push({
      sessionKey,
      sessionHash,
      account,
      host,
      firstMs,
      lastMs,
      model: account.models[s % account.models.length],
      cwdKey: sha256Hex(`cwd:${host}:${s % 12}`).slice(0, 16),
      cwdPath: `~/work/example-project-${pad((s % 12) + 1, 2)}`,
    });
  }

  let totalActivityEvents = 0;
  const activityFiles = [];
  for (const spec of PROVIDER_SPECS) {
    const providerAccounts = accountsByProvider.get(spec.id) ?? [];
    const providerSessions = sessionsPerProvider.get(spec.id) ?? [];
    const lines = [];
    // Index sessions by active hour for O(1) lookup, plus steady background hourly usage across all 60 days.
    const hourSessions = new Map();
    for (const sess of providerSessions) {
      for (let h = sess.firstMs; h <= sess.lastMs; h += HOUR) {
        if (!hourSessions.has(h)) hourSessions.set(h, []);
        hourSessions.get(h).push(sess);
      }
    }

    for (let h = startMs; h < now; h += HOUR) {
      const date = new Date(h);
      const hod = date.getUTCHours();
      const dow = date.getUTCDay();
      const workWave =
        Math.exp(-((hod - 15) ** 2) / 18) + 0.35 * Math.exp(-((hod - 21) ** 2) / 6);
      const weekend = dow === 0 || dow === 6 ? 0.5 : 1.0;
      const intensity = workWave * weekend * (0.6 + rng.next() * 0.8);
      if (intensity < 0.05) continue;

      const activeSess = hourSessions.get(h) ?? [];
      // Ensure every active hour has at least 2-3 (host, model, account) records so 7d/30d payloads match live density.
      const entries = [];
      for (const sess of activeSess) {
        entries.push({
          host: sess.host,
          account: sess.account,
          model: sess.model,
          sessionKey: sess.sessionKey,
          sessionHash: sess.sessionHash,
          cwdKey: sess.cwdKey,
          cwdPath: sess.cwdPath,
        });
      }
      const floorCount = Math.max(0, 4 - entries.length);
      for (let e = 0; e < floorCount; e += 1) {
        const account = providerAccounts[(Math.floor(h / HOUR) + e) % providerAccounts.length];
        const model = account.models[(Math.floor(h / HOUR) + e) % account.models.length];
        const host = HOSTS[(Math.floor(h / HOUR) + account.index + e) % HOSTS.length];
        const fallbackSess =
          providerSessions[(Math.floor(h / HOUR) + e) % Math.max(1, providerSessions.length)];
        entries.push({
          host,
          account,
          model,
          sessionKey: fallbackSess ? fallbackSess.sessionKey : null,
          sessionHash: fallbackSess ? fallbackSess.sessionHash : null,
          cwdKey: fallbackSess ? fallbackSess.cwdKey : null,
          cwdPath: fallbackSess ? fallbackSess.cwdPath : null,
        });
      }

      const hourIso = isoSec(h);
      for (const entry of entries) {
        const base = 1.6e6 * intensity * entry.model.weight * (0.65 + rng.next() * 0.7);
        const inputTokens = Math.max(500, Math.round(base));
        const outputTokens = Math.max(100, Math.round(base * (0.06 + rng.next() * 0.08)));
        const cacheCreationTokens =
          entry.model.cacheCreate > 0 ? Math.round(base * entry.model.cacheCreate) : 0;
        const cacheReadTokens = Math.round(base * entry.model.cacheK * (0.75 + rng.next() * 0.5));
        const costUsd = modelCostUsd(entry.model.id, {
          inputTokens,
          outputTokens,
          cacheCreationTokens,
          cacheReadTokens,
        });
        lines.push(
          JSON.stringify({
            hour: hourIso,
            provider: spec.id,
            tool: spec.tool,
            route: spec.route,
            host: entry.host,
            accountId: entry.account.id,
            accountKey: entry.account.identity,
            modelRaw: entry.model.id,
            sessionKey: entry.sessionKey,
            sessionHash: entry.sessionHash,
            cwdKey: entry.cwdKey,
            cwdPath: entry.cwdPath,
            inputTokens,
            outputTokens,
            cacheCreationTokens,
            cacheReadTokens,
            costUsd,
            events: Math.max(1, Math.round(inputTokens / 35_000)),
          })
        );
      }
    }

    const file = path.join(usageLogsDir, `activity-${spec.id}.jsonl`);
    fs.writeFileSync(file, `${lines.join('\n')}${lines.length ? '\n' : ''}`);
    totalActivityEvents += lines.length;
    activityFiles.push(file);
  }

  return {
    quotaFiles,
    activityFiles,
    quotaObservations,
    activityEvents: totalActivityEvents,
    sessions: activitySessions,
  };
}

/** Resolve the compiled native aac-store NAPI binding. */
export function resolveNativeStoreModule(explicitPath = null) {
  if (explicitPath) return path.resolve(explicitPath);
  const name = `aac-store.${process.platform}-${process.arch}.node`;
  const candidates = [
    path.join(REPO_ROOT, 'dist', 'native', name),
    path.join(REPO_ROOT, 'crates', 'aac-store', 'prebuilt', name),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate;
  }
  throw new Error(
    `Missing native store module (${name}). Run "bun run store:build" on a Rust build host first.`
  );
}

/** Migrate the generated quota logs, activity logs, and run files into <ccsDir>/store. */
async function migrateStoreFromFixture({
  ccsDir,
  reportsDir,
  quotaFiles,
  activityFiles,
  now,
  days,
  storeModule,
}) {
  const require = createRequire(import.meta.url);
  const modulePath = resolveNativeStoreModule(storeModule);
  const native = require(modulePath);
  const storeDir = path.join(ccsDir, 'store');
  fs.mkdirSync(storeDir, { recursive: true, mode: 0o700 });

  const handle = new native.NativeStore();
  const call = async (method, payload) =>
    JSON.parse(await handle.call(method, JSON.stringify(payload)));

  await call('open', { dir: storeDir, retentionDays: 0 });
  const stats = {
    quotaSampled: 0,
    quotaSeen: 0,
    quotaRejected: 0,
    activityReports: 0,
    activityCellsPut: 0,
    runsRecorded: 0,
    runsRefused: 0,
    runsPasses: 0,
  };

  try {
    // 1. Migrate quota observations from the generated daily quota logs.
    let batch = [];
    const flushQuota = async () => {
      if (!batch.length) return;
      const res = await call('record', { observations: batch });
      stats.quotaSampled += res.sampled ?? 0;
      stats.quotaSeen += res.seen ?? 0;
      stats.quotaRejected += res.rejected ?? 0;
      if ((res.rejected ?? 0) > 0) {
        throw new Error(`Store rejected ${res.rejected} synthetic quota observations`);
      }
      batch = [];
    };

    for (const file of quotaFiles) {
      const doc = JSON.parse(fs.readFileSync(file, 'utf8'));
      const label = path.basename(file);
      let maxDayMs = 0;
      for (let idx = 0; idx < doc.records.length; idx += 1) {
        const rec = doc.records[idx];
        const sampledMs = Date.parse(rec.sampledAt);
        const observedMs = Date.parse(rec.observedAt);
        if (sampledMs > maxDayMs) maxDayMs = sampledMs;
        batch.push({
          account: {
            identity: rec.identity,
            provider: rec.provider,
            account_id: rec.accountId,
            plan: rec.plan ?? null,
            platform: rec.platform,
            source: rec.source,
            status: rec.status,
            is_active: rec.isActive === true,
          },
          sampled_at_ms: sampledMs,
          observed_at_ms: observedMs,
          windows: rec.windows.map((w) => ({
            identity: windowIdentity(w),
            key: w.key,
            label: w.label,
            kind: w.kind ?? null,
            unit: w.unit ?? null,
            used_pct: w.usedPercent ?? null,
            remaining_pct: w.remainingPercent ?? null,
            used: w.used ?? null,
            limit: w.limit ?? null,
            remaining: w.remaining ?? null,
            reset_at_ms: w.resetAt ? Date.parse(w.resetAt) : null,
            expires_at_ms: null,
            window_minutes: w.windowMinutes ?? null,
            unlimited: null,
            enabled: null,
            cached_sampled_at_ms: null,
            pool_id: null,
            pool_id_source: null,
            pool_label: null,
            model_ids: null,
          })),
          import_ref: `${label}#${idx}`,
        });
        if (batch.length >= 2000) await flushQuota();
      }
      await flushQuota();
      if (maxDayMs > 0) {
        // Close each day incrementally as live daily maintenance does (avoids single-event frame cap).
        await call('maintain', { nowMs: Math.min(now, maxDayMs + 3 * HOUR) });
      }
    }

    // 2. Migrate hourly activity & sessions from the generated activity-*.jsonl usage logs.
    const coverFrom = isoSec(now - days * DAY);
    const readAt = isoSec(now);
    for (const file of activityFiles) {
      const raw = fs.readFileSync(file, 'utf8').trim();
      if (!raw) continue;
      // Group into (computer, tool) reports as aac.usage-report.v1 requires.
      const byHostTool = new Map();
      for (const line of raw.split('\n')) {
        if (!line) continue;
        const ev = JSON.parse(line);
        const key = `${ev.host}|${ev.tool}`;
        if (!byHostTool.has(key)) {
          byHostTool.set(key, {
            computer: ev.host,
            tool: ev.tool,
            cells: new Map(),
            sessions: new Map(),
          });
        }
        const bucket = byHostTool.get(key);
        const cellKey = `${ev.hour}|${ev.sessionKey ?? ''}|${ev.route}|${ev.modelRaw}|${ev.accountKey ?? ''}`;
        let cell = bucket.cells.get(cellKey);
        if (!cell) {
          cell = {
            hour: ev.hour,
            sessionKey: ev.sessionKey ?? null,
            sessionHash: ev.sessionHash ?? null,
            route: ev.route,
            modelRaw: ev.modelRaw,
            accountKey: ev.accountKey ?? null,
            accountSource: ev.accountKey ? 'fixture' : null,
            inputTokens: 0,
            outputTokens: 0,
            cacheCreationTokens: 0,
            cacheReadTokens: 0,
            costUsd: 0,
            fallbackCostUsd: 0,
            events: 0,
          };
          bucket.cells.set(cellKey, cell);
        }
        cell.inputTokens += ev.inputTokens;
        cell.outputTokens += ev.outputTokens;
        cell.cacheCreationTokens += ev.cacheCreationTokens;
        cell.cacheReadTokens += ev.cacheReadTokens;
        cell.costUsd = +(cell.costUsd + ev.costUsd).toFixed(6);
        cell.events += ev.events;

        if (ev.sessionKey) {
          let sess = bucket.sessions.get(ev.sessionKey);
          if (!sess) {
            sess = {
              sessionKey: ev.sessionKey,
              sessionHash: ev.sessionHash ?? null,
              first: ev.hour,
              last: ev.hour,
              parts: new Map(),
              target: null,
              cwd: ev.cwdKey && ev.cwdPath ? { key: ev.cwdKey, path: ev.cwdPath } : null,
            };
            bucket.sessions.set(ev.sessionKey, sess);
          }
          if (ev.hour < sess.first) sess.first = ev.hour;
          if (ev.hour > sess.last) sess.last = ev.hour;
          const partKey = `${ev.route}|${ev.modelRaw}`;
          let part = sess.parts.get(partKey);
          if (!part) {
            part = {
              route: ev.route,
              modelRaw: ev.modelRaw,
              inputTokens: 0,
              outputTokens: 0,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              costUsd: 0,
              fallbackCostUsd: 0,
            };
            sess.parts.set(partKey, part);
          }
          part.inputTokens += ev.inputTokens;
          part.outputTokens += ev.outputTokens;
          part.cacheCreationTokens += ev.cacheCreationTokens;
          part.cacheReadTokens += ev.cacheReadTokens;
          part.costUsd = +(part.costUsd + ev.costUsd).toFixed(6);
        }
      }

      const reports = [];
      for (const bucket of byHostTool.values()) {
        reports.push({
          schema: 'aac.usage-report.v1',
          computer: bucket.computer,
          tool: bucket.tool,
          readAt,
          state: 'ok',
          complete: true,
          coverFrom,
          kind: 'import',
          cells: [...bucket.cells.values()],
          sessions: [...bucket.sessions.values()].map((s) => ({
            ...s,
            parts: [...s.parts.values()].slice(0, 32),
          })),
        });
      }

      for (const report of reports) {
        const res = await call('activityIngest', { reports: [report] });
        if (res.refused?.length) {
          throw new Error(
            `Store refused synthetic activity report (${report.computer}/${report.tool}): ${JSON.stringify(res.refused)}`
          );
        }
        stats.activityReports += 1;
        stats.activityCellsPut += res.put ?? 0;
      }
    }

    // 3. Migrate agent-report runs into the store via runsScan + runsRecord until complete.
    let more = true;
    let firstPass = true;
    while (more) {
      const scan = await call('runsScan', { root: reportsDir, full: firstPass });
      firstPass = false;
      const files = [];
      const refused = [...(scan.refused ?? [])];
      for (const item of scan.files ?? []) {
        const text = typeof item.text === 'string' ? item.text : '';
        if (text.includes('\0')) {
          refused.push({
            path: item.path,
            size: item.size,
            mtimeMs: item.mtimeMs,
            reason: 'file contains NUL bytes',
            sha256: sha256Hex(text),
          });
          continue;
        }
        try {
          JSON.parse(text);
          files.push(item);
        } catch {
          refused.push({
            path: item.path,
            size: item.size,
            mtimeMs: item.mtimeMs,
            reason: 'not valid JSON',
            sha256: sha256Hex(text),
          });
        }
      }
      const recRes = await call('runsRecord', {
        files,
        refused,
        threads: scan.threads ?? {},
        catalog: {
          models: scan.catalog?.models ?? null,
          aliases: scan.catalog?.aliases ?? null,
        },
        nowMs: now,
      });
      stats.runsPasses += 1;
      stats.runsRecorded += files.length;
      stats.runsRefused += refused.length;
      more = Boolean(scan.more) && (files.length > 0 || refused.length > 0);
      if (!recRes || stats.runsPasses > 20) break;
    }

    await call('maintain', { nowMs: now, compact: true });
    const status = await call('status', {});
    stats.redbBytes = status.redbBytes ?? 0;
    stats.runsStatus = await call('runsStatus', {});
  } finally {
    await call('close', {});
  }

  return stats;
}

/** Generate the full synthetic fixture and optionally migrate its store. */
export async function generateMemoryFixture(outDir, userOptions = {}) {
  const started = Date.now();
  const options = { ...DEFAULT_OPTIONS, ...userOptions };
  const now =
    options.now !== null && options.now !== undefined
      ? Number(options.now)
      : Math.floor(Date.now() / HOUR) * HOUR;
  const root = path.resolve(outDir);
  const home = path.join(root, 'home');
  const ccsDir = path.join(home, '.ccs');
  const reportsDir = path.join(root, 'agent-reports');
  const registryDir = path.join(reportsDir, 'pm-mail-registry');
  const usageLogsDir = path.join(root, 'usage-logs');

  if (fs.existsSync(path.join(reportsDir, 'v1', 'writers'))) {
    throw new Error(`${root} already contains a generated fixture; pass an empty directory.`);
  }

  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const rng = createRng(options.seed);

  // Reuse the repo's existing Status and Scoreboard fixture helpers for base coverage.
  writeStatusFixture(reportsDir, {
    ccsDir: null,
    withKinds: true,
    withVideo: false,
    withQuota: true,
    now,
  });
  seedDeterministicStatusAnswers(reportsDir, ccsDir, options.seed, now);
  const baseScoreboard = writeScoreboardFixture(reportsDir, { now });

  const writerResult = generateWriters(
    reportsDir,
    rng,
    now,
    options.writers,
    options.runWriters
  );
  const runResult = generateRuns(
    reportsDir,
    writerResult.runWriters,
    rng,
    now,
    options.runs,
    options.days,
    baseScoreboard.runs.length
  );
  const registryResult = generateMailRegistry(reportsDir, rng, now, options.registryEntries);
  const pricingResult = generateModelsDevCache(
    ccsDir,
    rng,
    now,
    options.pricingProviders,
    options.pricingModelsPerProvider
  );
  const accounts = buildSyntheticAccounts(options.accounts);
  const analyticsResult = generateAnalyticsUsageLogs(
    usageLogsDir,
    accounts,
    rng,
    now,
    options.days,
    options.quotaSamplesPerDay,
    options.activitySessions
  );

  let storeStats = null;
  if (!options.skipStore) {
    storeStats = await migrateStoreFromFixture({
      ccsDir,
      reportsDir,
      quotaFiles: analyticsResult.quotaFiles,
      activityFiles: analyticsResult.activityFiles,
      now,
      days: options.days,
      storeModule: options.storeModule,
    });
  }

  const manifest = {
    schemaVersion: 1,
    seed: options.seed,
    now,
    nowIso: isoSec(now),
    root,
    home,
    ccsDir,
    reportsDir,
    registryDir,
    usageLogsDir,
    counts: {
      writers: writerResult.totalWriters,
      runWriters: writerResult.runWriters.length,
      runs: runResult.totalRuns,
      registryEntries: registryResult.totalRegistryEntries,
      providers: PROVIDER_SPECS.length,
      accounts: accounts.length,
      days: options.days,
      quotaObservations: analyticsResult.quotaObservations,
      activityEvents: analyticsResult.activityEvents,
      activitySessions: analyticsResult.sessions,
      pricingProviders: pricingResult.providers,
      pricingModels: pricingResult.models,
    },
    store: storeStats,
    generationMs: Date.now() - started,
  };
  writeJsonCompact(path.join(root, 'fixture-manifest.json'), manifest);
  return manifest;
}

function parseCli(argv) {
  const args = argv.slice(2);
  const options = {};
  let outDir = null;
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    const next = () => {
      i += 1;
      if (i >= args.length) throw new Error(`Missing value for ${arg}`);
      return args[i];
    };
    if (arg === '--seed') options.seed = Number(next());
    else if (arg === '--now') options.now = Number(next());
    else if (arg === '--runs') options.runs = Number(next());
    else if (arg === '--run-writers') options.runWriters = Number(next());
    else if (arg === '--writers') options.writers = Number(next());
    else if (arg === '--registry') options.registryEntries = Number(next());
    else if (arg === '--days') options.days = Number(next());
    else if (arg === '--accounts') options.accounts = Number(next());
    else if (arg === '--quota-per-day') options.quotaSamplesPerDay = Number(next());
    else if (arg === '--sessions') options.activitySessions = Number(next());
    else if (arg === '--skip-store') options.skipStore = true;
    else if (arg === '--store-module') options.storeModule = next();
    else if (arg === '-h' || arg === '--help') return { help: true };
    else if (!arg.startsWith('-') && !outDir) outDir = arg;
    else throw new Error(`Unknown argument: ${arg}`);
  }
  return { help: false, outDir, options };
}

async function main(argv) {
  const parsed = parseCli(argv);
  if (parsed.help || !parsed.outDir) {
    process.stderr.write(
      'Usage: node scripts/perf/memory-fixture.mjs <out-dir> [--seed 20261125] [--now <epochMs>] [--runs 7500] [--writers 2000] [--registry 4500] [--days 60] [--accounts 19] [--skip-store]\n'
    );
    return parsed.help ? 0 : 2;
  }
  const manifest = await generateMemoryFixture(parsed.outDir, parsed.options);
  process.stdout.write(
    `[OK] Generated synthetic fixture in ${manifest.generationMs} ms at ${manifest.root}\n` +
      `     writers=${manifest.counts.writers} runs=${manifest.counts.runs} registry=${manifest.counts.registryEntries} ` +
      `providers=${manifest.counts.providers} accounts=${manifest.counts.accounts} days=${manifest.counts.days} ` +
      `quotaObs=${manifest.counts.quotaObservations} activityEvents=${manifest.counts.activityEvents}\n` +
      (manifest.store
        ? `     store: redb=${(manifest.store.redbBytes / 1048576).toFixed(1)} MiB, ` +
          `quotaSampled=${manifest.store.quotaSampled}, activityCells=${manifest.store.activityCellsPut}, ` +
          `runsRecorded=${manifest.store.runsRecorded}\n`
        : '     store: skipped (--skip-store)\n')
  );
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main(process.argv)
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      process.stderr.write(`[X] ${error instanceof Error ? error.stack || error.message : String(error)}\n`);
      process.exitCode = 1;
    });
}
