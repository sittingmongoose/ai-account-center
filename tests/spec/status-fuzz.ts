/**
 * R2 fuzz: generated Status inputs through today's handler (`buildStatusView`), the M3 envelope read and the fed
 * path (status-feed.ts with the real service), byte for byte. Each seed writes its own share of agent reports
 * (thread, tasks and card documents, some malformed) that the real reader scans, then builds cases around that
 * snapshot: clocks with millisecond offsets, time zones (Casablanca, Lord Howe and Chatham included), collected
 * panels, usage windows, answers and sends, quota values at rounding boundaries, unusual strings. A served fed
 * answer must be byte-equal to TS's, ETag included; a decline is today's answer. No case may differ.
 *
 *   bun tests/spec/status-fuzz.ts [--seeds 301,302,303,304] [--cases 400]
 * Fixture data only, in a temporary CCS home; the service it starts is its own.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../..', import.meta.url));
const args = process.argv.slice(2);
const option = (name: string, fallback: string) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const seeds = option('seeds', '301,302,303,304').split(',').map(Number);
const perSeed = Number(option('cases', '400'));
const home = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-status-fuzz-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });

const { RustService } = await import('../../src/web-server/rust-service');
const { requestRust } = await import('../../src/web-server/rust-service-transport');
const { rustStatusInput } = await import('../../src/web-server/rust-read-input');
const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { QuotaReportReader } = await import('../../src/web-server/status/quota-report');
const { buildStatusView } = await import('../../src/web-server/status/status-view');
const { quotaDocument } = await import('./read-view-fixtures');
const { writeReport, threadDoc, tasksDoc, item, writeCard, cardDoc } =
  await import('../unit/web-server/status/status-test-helpers');
const { statusFed, revalidatedEtag } = await import('./status-fed-check');

const NOW = Date.parse('2026-11-06T16:30:00.000Z');
const ZONES = [
  'UTC',
  'America/New_York',
  'Asia/Kolkata',
  'Australia/Lord_Howe',
  'Pacific/Chatham',
  'Africa/Casablanca',
  'Europe/London',
  'invalid-fixture-zone',
];
const WORDS = [
  'plain',
  'é',
  'é',
  'Ünïcode words',
  '日本語',
  'emoji 🙂',
  'quote " back \\',
  'line\nbreak',
  ' separator',
  'tab\tstop',
  'ends.',
  'a.b.c',
  'Zeta',
  'alpha',
  'Alpha',
  ' padded ',
  '',
];
const NUMBERS = [
  0,
  -0,
  1,
  2.5,
  0.05,
  0.049999999999999996,
  0.15,
  1 / 3,
  0.1 + 0.2,
  33.333333333333336,
  99.995,
  100,
  1e-7,
  12345.678,
  1e15,
  1e21,
  4.68,
  17.774,
  0.004999999999999999,
];

let state = 1;
const random = () => {
  state = (state * 1664525 + 1013904223) >>> 0;
  return state / 2 ** 32;
};
const pick = <T>(list: readonly T[]): T => list[Math.floor(random() * list.length)];
const chance = (p: number) => random() < p;
const iso = (ms: number) => new Date(ms).toISOString();

const THREAD_STATES = ['working', 'blocked', 'waiting', 'idle', 'done', 'reviewing'];
const ITEM_STATES = ['todo', 'doing', 'landed', 'blocked', 'dropped', 'review', 'unknown'];
const CARD_KINDS = ['question', 'judgement', 'action', 'sample'];
const URGENCY = ['now', 'today', 'this-week', 'whenever'];

/** A share of `writers` writers, some documents malformed, scanned by the real reader. */
async function share(seed: number) {
  const dir = path.join(home, `share-${seed}`);
  const writers = 6 + Math.floor(random() * 10);
  for (let w = 0; w < writers; w++) {
    const writer = `${pick(['vm', 'mac', 'nas1', 'windows'])}.fuzz-${seed}-${w}`;
    const minutes = Math.floor(random() * 3000);
    writeReport(
      dir,
      writer,
      'thread.json',
      threadDoc(writer, {
        writtenAt: iso(NOW - minutes * 60_000 - Math.floor(random() * 1000)),
        state: pick(THREAD_STATES),
        doing: pick(WORDS) || 'Working',
        project: pick(['proj', 'other', 'p.q', '']),
        stream: pick(['S', 'T', 'Stream é', '']),
        ttlSeconds: chance(0.3) ? null : pick([60, 1800, 86400]),
        blockedOn: chance(0.2) ? { kind: pick(['owner', 'agent']), detail: pick(WORDS) } : null,
        links: chance(0.3) ? [{ label: 'Board', path: `share/${writer}/board.md` }] : [],
        ...(chance(0.05) ? { seq: 'bad' } : {}),
      })
    );
    if (chance(0.7))
      writeReport(
        dir,
        writer,
        'tasks.json',
        tasksDoc(
          writer,
          Array.from({ length: Math.floor(random() * 6) }, (_, i) =>
            item(`I${i}`, pick(ITEM_STATES), {
              updatedAt: iso(NOW - Math.floor(random() * 9e7)),
              title: pick(WORDS) || `Item ${i}`,
            })
          ),
          {
            project: pick(['proj', 'other', 'p.q']),
            projectLabel: chance(0.3) ? null : pick(WORDS) || 'Project',
            stream: pick(['S', 'T', 'U']),
            streamLabel: chance(0.3) ? null : pick(WORDS) || 'Stream',
            board: chance(0.3) ? `share/${writer}/board.md` : null,
          }
        )
      );
    for (let c = 0; c < Math.floor(random() * 3); c++)
      writeCard(
        dir,
        cardDoc(writer, `c${c}`, {
          kind: pick(CARD_KINDS),
          urgency: pick(URGENCY),
          title: pick(WORDS) || 'Question',
          askedAt: iso(NOW - Math.floor(random() * 9e7)),
          ...(chance(0.1) ? { recommendation: null } : {}),
        })
      );
  }
  fs.mkdirSync(path.join(dir, 'v1/quota'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'v1/quota/report.json'), JSON.stringify(quotaDocument()));
  const reader = new AgentReportsReader({ root: () => dir, now: () => NOW });
  await reader.scan();
  const quota = new QuotaReportReader({ root: () => dir, now: () => NOW });
  await quota.scan();
  return { reports: reader.snapshot(), quota: quota.snapshot() };
}

function collected(data: unknown, now: number) {
  if (chance(0.15)) return { data: null, asOf: null, error: null };
  const asOf = now - Math.floor(random() * 4e5);
  return {
    data,
    asOf,
    error: chance(0.2) ? { reason: pick(WORDS) || 'Down.', since: asOf - 60_000 } : null,
    ...(chance(0.2)
      ? { problems: [{ item: pick(['disk', 'cpu', 'memory']), reason: 'Fuzz problem.', since: asOf }] }
      : {}),
  };
}

function machine(now: number) {
  return collected(
    {
      cores: pick([2, 8, 56]),
      load1: pick(NUMBERS),
      busyPct: chance(0.2) ? null : pick(NUMBERS),
      memAvailableBytes: pick(NUMBERS) * 1024 ** 3,
      memTotalBytes: 32 * 1024 ** 3,
      disk: chance(0.2)
        ? null
        : { mount: '/', freeBytes: pick(NUMBERS) * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
      cpuTimes: null,
    },
    now
  );
}

function usage(now: number) {
  if (chance(0.25)) return null;
  return {
    sampledAt: now - Math.floor(random() * 3e5),
    staleAfterSeconds: pick([60, 120, 600]),
    accounts: Array.from({ length: 1 + Math.floor(random() * 3) }, (_, a) => ({
      id: `fuzz-account-${a}`,
      provider: pick(['codex', 'claude', 'cursor']),
      label: pick(WORDS) || `Account ${a}`,
      host: pick(['vm', 'mac']),
      active: chance(0.7),
      hidden: chance(0.1),
      status: pick(['ok', 'stale', 'error']),
      sampledAt: iso(now - Math.floor(random() * 3e5)),
      windows: Array.from({ length: 1 + Math.floor(random() * 3) }, (_, w) => ({
        label: pick(['Weekly', '5 hours', 'Monthly', pick(WORDS)]),
        kind: 'rate_limit',
        usedPercent: pick(NUMBERS),
        remaining: null,
        limit: null,
        unit: 'percent',
        resetAt: chance(0.2) ? null : iso(now + Math.floor(random() * 7e8) - 1e8 + w),
        resetPassed: chance(0.1),
        cached: chance(0.2),
        burn: null,
        sampledAt: iso(now - Math.floor(random() * 3e5)),
      })),
    })),
  };
}

function answers(reports: any, now: number) {
  const cards = reports.cards as any[];
  if (!cards.length || chance(0.4)) return { answers: [], sends: [] };
  const done = () => ({
    state: pick(['done', 'pending', 'failed']),
    at: iso(now - Math.floor(random() * 1e6)),
    detail: pick(WORDS) || null,
    attempts: Math.floor(random() * 3),
  });
  const entries = cards.slice(0, 1 + Math.floor(random() * cards.length)).map((row, i) => ({
    id: `fuzz-answer-${i}`,
    sendId: `fuzz-send-${i % 2}`,
    answer: {
      cardId: row.doc.id,
      cardSeq: row.doc.seq,
      cardSha256: row.sha256,
      kind: pick(['answer', 'note', 'object', 'done']),
      choice: ['A'],
      note: pick(WORDS) || null,
      via: 'deck',
      answeredAt: iso(now - Math.floor(random() * 1e6)),
      device: 'Fuzz',
    },
    card: row.doc,
    share: done(),
    doorbell: done(),
  }));
  return {
    answers: entries,
    sends: [0, 1].map((i) => ({ sendId: `fuzz-send-${i}`, summary: done() })),
  };
}

function caseOf(base: { reports: any; quota: any }) {
  const now = NOW + pick([0, 1, 499, 500, 999, 60_000, 86_400_000, 2_000_000, -3_600_000]);
  const reports = structuredClone(base.reports);
  if (chance(0.1)) reports.state = pick(['missing', 'error']);
  if (chance(0.2))
    reports.problems.push({ file: 'writers/fuzz/thread.json', reason: pick(WORDS) || 'Bad.', since: now - 6e4 });
  const quota = structuredClone(base.quota);
  if (quota.good && chance(0.6))
    for (const period of Object.values(quota.good.doc.streamEstimates ?? quota.good.doc.periods ?? {}) as any[])
      for (const row of period?.rows ?? []) if (chance(0.5)) row.value = pick(NUMBERS);
  const ids = ['vm', 'mac', 'windows', 'nas1', 'alexis'];
  const input: any = {
    now,
    timeZone: pick(ZONES),
    reports,
    registry: collected(
      Array.from({ length: Math.floor(random() * 4) }, (_, i) => ({
        name: pick(['mail-worker', 'aac-doorbell', `fuzz-${i}`, pick(WORDS) || 'n']),
        kind: pick(['codex', 'claude']),
        machine: pick(ids),
        about: pick(WORDS),
        lastSeen: iso(now - Math.floor(random() * 1e8)),
        readAt: now - Math.floor(random() * 6e4),
        problem: chance(0.1)
          ? {
              item: pick([`fuzz-${i}.json`, pick(WORDS)]),
              reason: 'Fuzz file.',
              since: now - 1e4,
              // Out of contract now and then (TS writes "undefined: ..."): Rust must decline, never differ.
              ...(chance(0.2) ? { item: undefined } : {}),
            }
          : null,
      })),
      now
    ),
    machines: Object.fromEntries(ids.map((id) => [id, machine(now)])),
    browsers: collected(
      [
        {
          machine: 'vm',
          browsers: Math.floor(random() * 4),
          recordings: Math.floor(random() * 2),
          gpuPct: pick(NUMBERS),
          sessions: [],
          readAt: now - 3e4,
          problem: null,
        },
      ],
      now
    ),
    simlock: { data: null, asOf: null, error: null },
    usage: usage(now),
    ...answers(reports, now),
    answerHistoryProblem: chance(0.1) ? "The answer history can't be read right now." : null,
    hiddenThreadNames: ['aac-doorbell'],
    quota,
  };
  if (chance(0.5))
    input.pinned = chance(0.3)
      ? { unavailable: 'the pinned threads have not been read yet' }
      : {
          asOfMs: now,
          head: pick(WORDS),
          counts: { pinned: 1, looping: 0, waitingOnYou: 0 },
          machineProblems: [],
          updates: [],
        };
  return input;
}

/** The first path where two JSON values differ (for the report; bodies are fixture data). */
function firstDifference(a: unknown, b: unknown, at = '$'): string {
  const x = JSON.parse(JSON.stringify(a));
  const y = b;
  if (JSON.stringify(x) === JSON.stringify(y)) return '';
  if (x && y && typeof x === 'object' && typeof y === 'object') {
    const keys = [...new Set([...Object.keys(x), ...Object.keys(y as object)])];
    if (JSON.stringify(Object.keys(x)) !== JSON.stringify(Object.keys(y as object)))
      return `${at} keys ${JSON.stringify(Object.keys(x))} vs ${JSON.stringify(Object.keys(y as object))}`;
    for (const key of keys) {
      const found = firstDifference((x as any)[key], (y as any)[key], `${at}.${key}`);
      if (found) return found;
    }
  }
  return `${at}: ${JSON.stringify(x)?.slice(0, 160)} vs ${JSON.stringify(y)?.slice(0, 160)}`;
}

const service = new RustService('on');
const report: Record<string, any> = { seeds: {}, differences: [] as string[] };
try {
  await service.start(process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'dist/native/aac-http'));
  const boot = JSON.parse(fs.readFileSync(path.join(ccs, 'run/aac-http.json'), 'utf8'));
  const active = new Set<import('node:http').ClientRequest>();
  for (const seed of seeds) {
    state = seed;
    const base = await share(seed);
    const counts = { cases: 0, tsThrows: 0, fedServed: 0, fedDeclined: 0, envelopeServed: 0 };
    for (let i = 0; i < perSeed; i++) {
      const input = caseOf(base);
      counts.cases++;
      let expected: unknown;
      try {
        expected = buildStatusView(input);
      } catch {
        counts.tsThrows++;
        continue;
      }
      const text = JSON.stringify(expected);
      const envelope = await requestRust(
        boot,
        '/api/status',
        { kind: 'session', id: 'fixture-admin', role: 'admin' },
        active,
        rustStatusInput(input)
      );
      if (envelope.status === 200) {
        counts.envelopeServed++;
        if (JSON.stringify(envelope.body) !== text)
          report.differences.push(
            `seed ${seed} case ${i}: the envelope read differs at ${firstDifference(expected, envelope.body)}`
          );
      }
      const fed = await statusFed(service, input);
      if (!fed.served) {
        counts.fedDeclined++;
        continue;
      }
      counts.fedServed++;
      if (fed.bytes?.toString('utf8') !== text || fed.etag !== revalidatedEtag(expected))
        report.differences.push(`seed ${seed} case ${i}: the fed bytes differ`);
    }
    report.seeds[seed] = counts;
  }
  report.declined = service.diagnostics().routes['/api/status']?.declined ?? {};
  console.log(JSON.stringify(report, null, 1));
  assert.deepEqual(report.differences, [], 'no fuzzed Status case differs from TS');
  const served = Object.values(report.seeds).reduce((n: number, c: any) => n + c.fedServed, 0);
  assert.ok(served > 0, 'the fuzz serves cases through the fed path');
  console.log(`[OK] status-fuzz: ${seeds.length} seeds, ${served} fed answers byte-equal, 0 differences`);
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
