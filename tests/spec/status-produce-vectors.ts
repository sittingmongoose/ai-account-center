/**
 * G16c parity vectors: the Status feed producers, recorded from the real TypeScript code so
 * `crates/aac-status/tests/status_produce.rs` can require the Rust producers' bytes to be equal.
 *
 * Run from the repository root: `bun tests/spec/status-produce-vectors.ts` (it rewrites
 * `status-produce-vectors.json` beside this file).
 *
 * The six fed generations are recorded by calling the real `answerStatusFed` with a stand-in
 * service, exactly as `tests/unit/web-server/status-feed.test.ts` does; the parsers are the real
 * `status-collectors.ts` functions; the answers merge is the real `mergeSharedAnswers`; the facts
 * are the real envelope (`rustReadInput`) over the whole input. The display zone's offsets are
 * recorded with `serverZoneFacts`, so the Rust side writes its words from the same facts.
 */
import fs from 'fs';
import os from 'os';
import path from 'path';
import type { Request, Response } from 'express';
import { RustService } from '../../src/web-server/rust-service';
import {
  parseBrowserSlots,
  mergeBrowserRows,
  parseSimlockStatus,
  parseMachineProbe,
  busyBetween,
  type BrowserMachine,
  type MachineSample,
  type SimlockStatus,
} from '../../src/web-server/status/status-collectors';
import { mergeSharedAnswers, type AnswerHistoryEntry } from '../../src/web-server/status/answers';
import { answerStatusFed } from '../../src/web-server/status/status-feed';
import { buildStatusView, type StatusInputs } from '../../src/web-server/status/status-view';
import { SPEND_BY_STREAM_UNAVAILABLE } from '../../src/web-server/status/spend-by-stream';
import { AgentReportsReader } from '../../src/web-server/status/agent-reports-reader';
import { PinnedThreadsCollector } from '../../src/web-server/status/pinned-threads';
import type { NativeStatusHandle } from '../../src/status-native/native-loader';
import { ago, cardDoc, item, tasksDoc, threadDoc } from '../unit/web-server/status/status-test-helpers';
import { serverZoneFacts } from '../../src/web-server/views/home-zone';
import type { MachineId } from '../../src/web-server/status/report-schema';
import type { Collected } from '../../src/web-server/status/status-view';

const NOW = 1_790_000_000_000;
const TIME_ZONE = 'America/New_York';
const MACHINES: readonly MachineId[] = ['vm', 'mac', 'windows', 'nas1', 'alexis'];

/** The real service's generation rules, in memory; `fedView` answers the stand-in view. */
class StandIn extends RustService {
  boot = 'c'.repeat(24);
  held = new Map<string, { generation: string; data: string; facts: string }>();
  puts: { source: string; generation: string; data: string; facts: string }[] = [];
  constructor() {
    super('on');
  }
  override feedBoot(scope: string): string | null {
    return scope === SCOPE ? this.boot : null;
  }
  override fedDeclined(): void {}
  override async pushFeed(
    boot: string,
    source: string,
    generation: string,
    data: string,
    facts: string
  ): Promise<string | null> {
    if (boot !== this.boot) return null;
    this.puts.push({ source, generation, data, facts });
    const current = this.held.get(source);
    if (!current || Number(generation) > Number(current.generation))
      this.held.set(source, { generation, data, facts });
    return this.held.get(source)?.generation ?? null;
  }
  override async fedView(
    _req: Request,
    _route: string,
    _headers: Record<string, string>,
    _view: string
  ): Promise<{ bytes: Buffer; etag: string; view: string }> {
    return { bytes: Buffer.from('{"version":1}'), etag: `W/"${'a'.repeat(64)}"`, view: 'status@1' };
  }
}

let SCOPE = '';

/** The response fields `sendRustView` writes; the stand-in never sends a body. */
function response(): Response {
  return {
    req: { get: () => undefined },
    setHeader: () => undefined,
    vary: () => undefined,
    status: () => ({ end: () => undefined }),
    send: () => undefined,
  } as unknown as Response;
}

function collected<T>(data: T): Collected<T> {
  return { data, asOf: NOW, error: null };
}

/** `browser-check slots --json` as the fixture machines answer it. */
const SLOTS = {
  rows: [
    {
      machine: { name: 'vm', label: 'Ubuntu VM' },
      state: {
        caps: { max_browsers: 4, max_recordings: 2 },
        browsers: 1,
        recordings: 0,
        gpu_util: 12,
        mem_gb: 15.44,
        phones: null,
        sessions: [
          {
            id: 'abc123',
            url: 'http://10.0.0.5:8963/head.html?cb=1#x',
            owner: 'pid 5',
            recording: true,
            age_s: 466,
          },
        ],
      },
    },
  ],
};

/** A host browser-check could not read: its last good counts stay with the failure. */
const FAILED_SLOTS = {
  rows: [
    {
      machine: { name: 'vm', label: 'Ubuntu VM' },
      state: { error: 'gone', caps: { max_browsers: 4 }, browsers: 1, recordings: 0, sessions: [] },
    },
  ],
};

/** A host whose counts carry browser-check's own note (`snapshot`). */
const NOTED_SLOTS = {
  rows: [
    {
      machine: { name: 'vm', label: 'Ubuntu VM' },
      state: {
        caps: { max_browsers: 4 },
        browsers: 1,
        recordings: 0,
        sessions: [],
        snapshot: 'backend busy',
      },
    },
  ],
};

/** The fixture's browser reading, merged once so both sides start from the same state. */
const BROWSERS = mergeBrowserRows(null, parseBrowserSlots(SLOTS), NOW);


const SIMLOCK_INPUT = {
  daemon: { health: 'running', mode: 'gateway' },
  capacity: { ios: { running: 1, maxRunning: 1, reserved: 0, overLimit: false } },
  devices: [{ id: 'dev_1', spec: { platform: 'ios', model: 'iPhone Duo' } }],
  leases: [
    {
      deviceId: 'dev_1',
      requesterId: 'agent-7',
      grantedAt: NOW - 60_000,
      ttlDeadline: NOW + 60_000,
    },
  ],
};

const SIMLOCK = parseSimlockStatus({
  daemon: { health: 'running', mode: 'gateway' },
  capacity: { ios: { running: 1, maxRunning: 1, reserved: 0, overLimit: false } },
  devices: [{ id: 'dev_1', spec: { platform: 'ios', model: 'iPhone Duo' } }],
  leases: [
    {
      deviceId: 'dev_1',
      requesterId: 'agent-7',
      grantedAt: NOW - 60_000,
      ttlDeadline: NOW + 60_000,
    },
  ],
});

/** The fixture input, assembled from real reader snapshots where a reader owns the shape. */
async function input(): Promise<StatusInputs> {
  const share = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-produce-vectors-'));
  fs.mkdirSync(path.join(share, 'v1/writers/vm.lead/cards'), { recursive: true });
  const write = (name: string, doc: unknown) =>
    fs.writeFileSync(
      path.join(share, 'v1/writers/vm.lead', name),
      `${JSON.stringify(doc, null, 2)}\n`
    );
  write(
    'thread.json',
    threadDoc('vm.lead', {
      seq: 3,
      writtenAt: ago(60_000, NOW),
      doing: 'Porting the producers',
      goal: 'Keep pinned deadlines visible.',
      nextBy: '2026-09-22T04:00:00.000Z',
      nextByDay: '2026-09-21',
      project: 'aac',
      stream: 'status',
      ttlSeconds: 900,
    })
  );
  write(
    'tasks.json',
    tasksDoc('vm.lead', [item('T1', 'landed', {
      why: 'The composer needs the date and the card link.',
      due: '2026-09-22T04:00:00.000Z',
      dueDay: '2026-09-21',
      dueNote: 'Check the pinned view.',
      dueWas: '2026-09-21T04:00:00.000Z',
      doneAt: '2026-09-21T14:00:00.000Z',
      outcome: 'The fixture is ready.',
      needsYou: 'vm.lead--Q1',
    })], {
      seq: 2,
      writtenAt: ago(60_000, NOW),
      project: 'aac',
      projectLabel: 'AAC',
      stream: 'status',
      streamLabel: 'Status',
    })
  );
  write(
    'cards/vm.lead--Q1.json',
    cardDoc('vm.lead', 'Q1', {
      writtenAt: ago(60_000, NOW),
      project: 'aac',
      stream: 'status',
    })
  );
  const reader = new AgentReportsReader({ root: () => share, now: () => NOW });
  await reader.scan();
  const reports = reader.snapshot();
  reports.root = '/fixture/reports';
  reports.scannedAt = NOW;
  for (const rows of [reports.threads, reports.tasks, reports.cards])
    for (const row of rows) row.firstSeenAt = NOW;

  const browserRows = BROWSERS;
  const machines = Object.fromEntries(
    MACHINES.map((machine) => [
      machine,
      collected<MachineSample>({
        cores: 8,
        load1: 1.2,
        busyPct: machine === 'vm' ? 12 : null,
        memAvailableBytes: 6 * 1024 ** 3,
        memTotalBytes: 16 * 1024 ** 3,
        disk: { mount: '/', freeBytes: 100 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
        cpuTimes: null,
        cpuReadAt: machine === 'vm' ? NOW : null,
        diskReadAt: NOW,
        memoryReadAt: NOW,
      }),
    ])
  ) as Record<MachineId, Collected<MachineSample>>;
  // The fixture's typed input; the casts on the two document rows carry the reader's own
  // snapshot objects, whose optional members the fixture leaves as the reader wrote them.
  return {
    now: NOW,
    timeZone: TIME_ZONE,
    reports,
    registry: {
      data: [
        {
          name: 'mail-worker',
          kind: 'codex',
          machine: 'vm',
          about: 'Fixture task',
          lastSeen: new Date(NOW - 60_000).toISOString(),
          readAt: NOW - 30_000,
          problem: null,
        },
      ],
      asOf: NOW,
      error: null,
    },
    machines,
    browsers: { ...collected<BrowserMachine[]>(browserRows), problems: [] },
    simlock: { ...collected<SimlockStatus>(SIMLOCK), problems: [] },
    usage: {
      accounts: [
        {
          id: 'claude-max',
          provider: 'claude',
          label: 'Max',
          host: 'ubuntu',
          active: true,
          hidden: false,
          status: 'ok',
          sampledAt: '2026-09-21T14:10:00.000Z',
          windows: [],
        },
      ],
      sampledAt: NOW,
      staleAfterSeconds: 180,
    },
    answers: [],
    sends: [],
    answerHistoryProblem: null,
    hiddenThreadNames: ['aac-answers'],
    quota: null,
    pinned: { unavailable: 'the pinned threads have not been read yet' },
    spendByStream: SPEND_BY_STREAM_UNAVAILABLE,
  } as unknown as StatusInputs;
}

function entry(id: string, cardId: string, sha: string, at: string, shared: boolean): AnswerHistoryEntry {
  return {
    id,
    sendId: null,
    ...(shared ? { sharedOnly: true as const } : {}),
    // The journal's signed answer, as `mergeSharedAnswers` reads it; the fixture writes only
    // the fields the merge and the page use, so the cast carries the test's own literal.
    answer: {
      kind: 'answer',
      cardId,
      cardSeq: 1,
      cardSha256: sha,
      answeredAt: at,
    } as AnswerHistoryEntry['answer'],
    card: { id: cardId, seq: 1 } as AnswerHistoryEntry['card'],
    share: { state: 'done', at: null, detail: null, attempts: 1 },
    doorbell: { state: 'skipped', at: null, detail: null, attempts: 0 },
  };
}

/** The pinned collector's native calls, recorded so the Rust input can be compared. */
async function pinnedInput(value: StatusInputs): Promise<{ input: unknown; pins: unknown }> {
  const ids = ['t1'];
  const pins = [
    {
      id: 't1',
      title: 'Pinned',
      pinnedAt: '2026-09-20T00:00:00.000Z',
      pinOrderKey: 'a',
      machine: 'vm',
    },
  ];
  let captured: unknown = null;
  const handle = {
    async call(method: string, args: string): Promise<string> {
      if (method === 'pins')
        return JSON.stringify({ pinned: [{ id: 't1', title: 'Pinned', pinnedAt: '2026-09-20T00:00:00.000Z', pinOrderKey: 'a' }], notShown: 0 });
      if (method === 'parseRemotePins') return JSON.stringify({ ok: true, pinned: [], notShown: 0 });
      if (method === 'mergePins') return JSON.stringify({ pinned: pins });
      if (method === 'compose') {
        captured = JSON.parse(args);
        return JSON.stringify({ head: '1 pinned', updates: [] });
      }
      return JSON.stringify({ ok: false });
    },
  } as unknown as NativeStatusHandle;
  const collector = new PinnedThreadsCollector({
    handle,
    run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
    readRemote: async () => '[]',
    now: () => NOW,
    reportsRoot: () => value.reports.root,
    readersCheck: null,
  });
  await collector.collect();
  await collector.compose(value.reports, [], collector.docs.snapshot());
  return { input: captured, pins };
}

async function main(): Promise<void> {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-produce-scope-'));
  process.env.CCS_DIR = path.join(home, '.ccs');
  SCOPE = process.env.CCS_DIR;
  const service = new StandIn();
  const value = await input();
  const req = {
    method: 'GET',
    originalUrl: '/api/status',
    headers: {},
  } as unknown as Request;
  // The stand-in records every source's generation; the real envelope is what Rust must match.
  const answered = await answerStatusFed(req, response(), service, value, SCOPE);
  if (!answered) throw new Error('the fed Status path declined the fixture');
  const sources: Record<string, { data: string; facts: string }> = {};
  for (const put of service.puts) sources[put.source] = { data: put.data, facts: put.facts };
  const zone = serverZoneFacts(TIME_ZONE, NOW);
  if (!zone) throw new Error('the fixture zone is unknown to this engine');
  const local = [entry('a', 'vm.lead--Q1', 's1', '2026-09-21T14:00:00.000Z', false)];
  const shared = [
    entry('b', 'vm.lead--Q1', 's1', '2026-09-21T14:00:00.000Z', true),
    entry('c', 'vm.lead--Q2', 's2', '2026-09-21T13:00:00.000Z', true),
  ];
  const pinned = await pinnedInput(value);
  const out = {
    now: NOW,
    pinned: { now: NOW, ...pinned },
    timeZone: TIME_ZONE,
    zone,
    input: value,
    sources,
    view: JSON.stringify(buildStatusView(value)),
    browsers: {
      slots: SLOTS,
      now: NOW,
      rows: parseBrowserSlots(SLOTS),
      before: BROWSERS,
      failed: FAILED_SLOTS,
      merged: mergeBrowserRows(BROWSERS, parseBrowserSlots(FAILED_SLOTS), NOW + 1_000),
      noted: NOTED_SLOTS,
      problems: mergeBrowserRows(BROWSERS, parseBrowserSlots(NOTED_SLOTS), NOW + 2_000),
    },
    simlock: { input: SIMLOCK_INPUT, expected: SIMLOCK },
    machines: {
      probes: (
        [
          {
            host: 'linux',
            stdout:
              'cores=6\nload=7.48\nmemTotalKb=20210000\nmemAvailKb=13245000\ncpu=100 0 50 800 50 0 0 0\ndiskTotalKb=812506724\ndiskFreeKb=528836920\nmount=/\n',
          },
          {
            host: 'mac',
            stdout:
              'cores=8\nload=2.84\nmemTotalB=17179869184\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=482766932\ndiskFreeKb=18378524\nmount=/System/Volumes/Data\n',
          },
          {
            host: 'windows',
            stdout:
              'cores=32\r\ncpuPct=40\r\nmemTotalKb=33463884\r\nmemAvailKb=10291080\r\ndiskTotalB=1999342923776\r\ndiskFreeB=218592583680\r\nmount=C:\r\n',
          },
        ] as const
      ).map((probe) => ({
        ...probe,
        expected: parseMachineProbe(probe.host, probe.stdout),
      })),
      busy: [
        {
          before: { busy: 150, total: 1000 },
          after: { busy: 250, total: 1200 },
          expected: busyBetween({ busy: 150, total: 1000 }, { busy: 250, total: 1200 }),
        },
        { before: null, after: { busy: 250, total: 1200 }, expected: null },
      ],
    },
    answersMerge: { local, shared, merged: mergeSharedAnswers(local, shared) },
  };
  fs.writeFileSync(
    path.resolve(import.meta.dir, 'status-produce-vectors.json'),
    `${JSON.stringify(out, null, 2)}\n`
  );
  console.log(
    `wrote status-produce-vectors.json (${Object.keys(sources).length} sources, ${out.view.length} view bytes)`
  );
}

await main();
