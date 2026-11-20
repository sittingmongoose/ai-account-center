/**
 * R2 fuzz: generated Status inputs through today's handler (`buildStatusView`), the M3 envelope read and the fed
 * path (status-feed.ts with the real service), byte for byte. Each seed writes its own share of agent reports
 * (thread, tasks and card documents, some malformed) that the real reader scans, then builds cases around that
 * snapshot: clocks with millisecond offsets, time zones (Casablanca, Lord Howe and Chatham included), collected
 * panels, usage windows, answers and sends, quota values at rounding boundaries, unusual strings. A served fed
 * answer must be byte-equal to TS's, ETag included; a decline is today's answer. No case may differ. After the
 * seeds, a persisted rich same-generation clock sweep (R2b L1) queries one share at +0/+1 ms, +30 s/+30 s+1 ms,
 * +90 s/+90 s+1 ms and +120 s/+120 s+1 ms, with a usage window projected full at +90 s and resetting at +120 s:
 * cache hits across moved clocks, fill-only `generatedAt`, and crossed span/full/reset decisions.
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
      ? {
          problems: [
            { item: pick(['disk', 'cpu', 'memory']), reason: 'Fuzz problem.', since: asOf },
          ],
        }
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
    reports.problems.push({
      file: 'writers/fuzz/thread.json',
      reason: pick(WORDS) || 'Bad.',
      since: now - 6e4,
    });
  const quota = structuredClone(base.quota);
  if (quota.good && chance(0.6))
    for (const period of Object.values(
      quota.good.doc.streamEstimates ?? quota.good.doc.periods ?? {}
    ) as any[])
      for (const row of period?.rows ?? []) if (chance(0.5)) row.value = pick(NUMBERS);
  const ids = ['vm', 'mac', 'windows', 'nas1', 'alexis', 'odroid', '1070ti'];
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

/**
 * R2b L1: the rich same-generation clock sweep's share, scanned once at `base`: a 60 s TTL thread, a day-TTL
 * blocked thread, a board with a fractional mtime, one open question, and the quota fixture. Deterministic;
 * it draws no random numbers, so the sweep is the same on every run.
 */
async function sweepShare(base: number, label: string) {
  const dir = path.join(home, `sweep-${label}`);
  const board = path.join(dir, 'v1', 'writers', 'vm.sweep', 'board.md');
  writeReport(
    dir,
    'vm.sweep',
    'thread.json',
    threadDoc('vm.sweep', {
      writtenAt: iso(base - 10_000),
      ttlSeconds: 60,
      doing: 'Sweeping the clock',
    })
  );
  writeReport(
    dir,
    'mac.sweep',
    'thread.json',
    threadDoc('mac.sweep', {
      writtenAt: iso(base - 3_600_000),
      ttlSeconds: 86_400,
      state: 'blocked',
      doing: 'Held for a day',
      blockedOn: { kind: 'owner', detail: 'A decision.' },
    })
  );
  writeReport(
    dir,
    'vm.sweep',
    'tasks.json',
    tasksDoc('vm.sweep', [item('A', 'doing', { updatedAt: iso(base - 20_000), title: 'Sweep item' })], {
      board,
      writtenAt: iso(base - 15_000),
    })
  );
  fs.mkdirSync(path.dirname(board), { recursive: true });
  fs.writeFileSync(board, '# Board\n');
  // A fractional mtime: the board age probe keeps its rounded second across +1 ms, and moves at +30 s.
  const mtime = (base - 45_678.25) / 1000;
  fs.utimesSync(board, mtime, mtime);
  writeCard(
    dir,
    cardDoc('vm.sweep', 'q', {
      kind: 'question',
      writtenAt: iso(base - 60_000),
      askedAt: iso(base - 600_000),
    })
  );
  fs.mkdirSync(path.join(dir, 'v1/quota'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'v1/quota/report.json'), JSON.stringify(quotaDocument()));
  const reader = new AgentReportsReader({ root: () => dir, now: () => base });
  await reader.scan();
  const quota = new QuotaReportReader({ root: () => dir, now: () => base });
  await quota.scan();
  return { reports: reader.snapshot(), quota: quota.snapshot() };
}

/**
 * The sweep's input at `base`: a fresh usage sample whose window is projected full at base+90 s and resets at
 * base+120 s, collected panels, one answered card. Every stored instant is fixed at `base`; only `input.now`
 * moves between queries, so the six generation ids stay the same and the cache is exercised across the moves.
 */
function sweepInput(shared: { reports: any; quota: any }, base: number) {
  const panel = (data: unknown) => ({ data, asOf: base - 30_000, error: null });
  const sample = {
    cores: 8,
    load1: 1.25,
    busyPct: 12.5,
    memAvailableBytes: 16 * 1024 ** 3,
    memTotalBytes: 32 * 1024 ** 3,
    disk: { mount: '/', freeBytes: 200 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
    cpuTimes: null,
  };
  const card = shared.reports.cards[0];
  const done = (at: number) => ({ state: 'done', at: iso(at), detail: null, attempts: 1 });
  const input: any = {
    now: base,
    timeZone: 'UTC',
    reports: shared.reports,
    registry: panel([
      {
        name: 'sweep-worker',
        kind: 'codex',
        machine: 'vm',
        about: 'The clock sweep',
        lastSeen: iso(base - 60_000),
        readAt: base - 30_000,
        problem: null,
      },
    ]),
    machines: Object.fromEntries(
      ['vm', 'mac', 'windows', 'nas1', 'alexis', 'odroid', '1070ti'].map((id) => [id, panel(structuredClone(sample))])
    ),
    browsers: panel([
      {
        machine: 'vm',
        browsers: 1,
        recordings: 0,
        gpuPct: 25,
        sessions: [],
        readAt: base - 30_000,
        problem: null,
      },
    ]),
    simlock: { data: null, asOf: null, error: null },
    usage: {
      sampledAt: base - 5_000,
      staleAfterSeconds: 600,
      accounts: [
        {
          id: 'sweep-account',
          provider: 'codex',
          label: 'Sweep',
          host: 'vm',
          active: true,
          hidden: false,
          status: 'ok',
          sampledAt: iso(base - 5_000),
          windows: [
            {
              label: '5 hours',
              kind: 'rate_limit',
              usedPercent: 45.25,
              remaining: null,
              limit: null,
              unit: 'percent',
              resetAt: iso(base + 120_000),
              resetPassed: false,
              cached: false,
              burn: { projectedFullAt: iso(base + 90_000), runsOutBeforeReset: true },
              sampledAt: iso(base - 5_000),
            },
          ],
        },
      ],
    },
    answers: [
      {
        id: 'sweep-answer',
        sendId: 'sweep-send',
        answer: {
          cardId: card.doc.id,
          cardSeq: card.doc.seq,
          cardSha256: card.sha256,
          kind: 'answer',
          choice: ['A'],
          note: null,
          via: 'deck',
          answeredAt: iso(base - 120_000),
          device: 'Sweep',
        },
        card: card.doc,
        share: done(base - 110_000),
        doorbell: done(base - 105_000),
      },
    ],
    sends: [{ sendId: 'sweep-send', summary: done(base - 100_000) }],
    answerHistoryProblem: null,
    hiddenThreadNames: ['aac-doorbell'],
    quota: shared.quota,
    pinned: { unavailable: 'the pinned threads have not been read yet' },
  };
  return input;
}

/** The revalidated-view identity of a serialized view: the ETag's own exclusions. */
const identityOf = (text: string) =>
  JSON.stringify(JSON.parse(text), (key, value: unknown) =>
    key === 'generatedAt' || key === 'ageSeconds' || key === 'cacheAgeSeconds' ? undefined : value
  );

/**
 * R2b L1: the persisted rich same-generation clock sweep. Queries at +0/+1 ms, +30 s/+30 s+1 ms,
 * +90 s/+90 s+1 ms and +120 s/+120 s+1 ms over one share: every fed answer must equal a fresh TS composition
 * byte for byte, ETag included; the same instant twice must answer byte-identical (the kept view); a +1 ms
 * step over a composed instant may move only `generatedAt`; and a crossed span, full projection or reset must
 * change the identity (the kept bytes are not served across a decision). The midnight phase crosses the day
 * word at +30 s; the afternoon phase crosses minute words at +90 s and +120 s.
 */
async function sweepClocks(service: RustService) {
  const sweep: Record<string, any> = {};
  for (const [label, base] of [
    ['afternoon', NOW],
    ['midnight', Date.parse('2026-11-06T23:59:30.000Z')],
  ] as const) {
    const shared = await sweepShare(base, label);
    const input = sweepInput(shared, base);
    const views = new Map<number, { text: string; etag: string }>();
    for (const offset of [0, 1, 30_000, 30_001, 90_000, 90_001, 120_000, 120_001]) {
      input.now = base + offset;
      const expected = buildStatusView(input);
      const text = JSON.stringify(expected);
      const fed = await statusFed(service, input);
      assert.ok(fed.served, `the sweep ${label} +${offset} ms serves through the fed path`);
      assert.equal(fed.bytes?.toString('utf8'), text, `the sweep ${label} +${offset} ms fed bytes`);
      assert.equal(fed.etag, revalidatedEtag(expected), `the sweep ${label} +${offset} ms fed ETag`);
      const again = await statusFed(service, input);
      assert.ok(again.served, `the sweep ${label} +${offset} ms repeat serves`);
      assert.equal(
        again.bytes?.toString('utf8'),
        text,
        `the sweep ${label} +${offset} ms repeat answers the kept bytes`
      );
      assert.equal(again.etag, fed.etag, `the sweep ${label} +${offset} ms repeat ETag`);
      views.set(offset, { text, etag: fed.etag! });
    }
    for (const [composed, hit] of [
      [0, 1],
      [90_000, 90_001],
      [120_000, 120_001],
    ] as const) {
      const a = views.get(composed)!;
      const b = views.get(hit)!;
      assert.equal(
        identityOf(b.text),
        identityOf(a.text),
        `the sweep ${label} +${hit} ms keeps the composed identity`
      );
      assert.equal(b.etag, a.etag, `the sweep ${label} +${hit} ms keeps the ETag`);
      assert.ok(
        b.text.includes(`"generatedAt":"${iso(base + hit)}"`),
        `the sweep ${label} +${hit} ms fills generatedAt`
      );
    }
    for (const [before, after] of [
      [30_000, 30_001],
      [0, 90_000],
      [90_000, 120_000],
    ] as const) {
      assert.notEqual(
        identityOf(views.get(after)!.text),
        identityOf(views.get(before)!.text),
        `the sweep ${label} +${after} ms crosses a recorded decision`
      );
    }
    sweep[label] = { base: iso(base), queries: views.size, answers: views.size * 2 };
  }
  return sweep;
}

const service = new RustService('on');
const report: Record<string, any> = { seeds: {}, differences: [] as string[] };
try {
  await service.start(
    process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'dist/native/aac-http')
  );
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
  report.sweep = await sweepClocks(service);
  report.declined = service.diagnostics().routes['/api/status']?.declined ?? {};
  console.log(JSON.stringify(report, null, 1));
  assert.deepEqual(report.differences, [], 'no fuzzed Status case differs from TS');
  const served = Object.values(report.seeds).reduce((n: number, c: any) => n + c.fedServed, 0);
  assert.ok(served > 0, 'the fuzz serves cases through the fed path');
  console.log(
    `[OK] status-fuzz: ${seeds.length} seeds, ${served} fed answers byte-equal, 0 differences; ` +
      `clock sweep ${Object.values(report.sweep).reduce((n: number, p: any) => n + p.answers, 0)} answers across cache hits`
  );
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
