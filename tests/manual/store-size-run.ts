/**
 * The store's size acceptance run (docs/store.md 11.4): 16 accounts with 4 windows each, a sample
 * every minute, daily maintenance and a final compaction, against the JSON history of the same
 * accounts. Also times the queries, hashes their answers (so two builds can be compared byte for
 * byte) and rebuilds redb from the logs. Everything lives in a temporary folder.
 *
 *   bun run store:build
 *   bun tests/manual/store-size-run.ts
 *
 * Environment: DAYS (30); FRACTION=1 (every percentage non-round) or MIX=1 (10 of the 64 series, 5/32
 * as in the published snapshot, with 4-decimal percentages that change every sample); STEADY=1 (seal
 * and compress the log before the last day, so the raw tail is one real day, as a store sealing at
 * midnight carries; without it the replay's wall-clock segments leave up to 8 MiB raw); WORST_ONLY=1
 * (only the worst-case query); MODULE (a .node file; default dist/native); OUT (a folder for
 * results.json and answers).
 */
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { loadNativeStore, nativeModuleName } from '../../src/store/native-loader';
import { AacStore } from '../../src/store/store-service';
import { storeObservationFromAccount } from '../../src/store/usage-observations';
import { appendAccountAnalyticsSnapshot } from '../../src/web-server/services/account-analytics-history';

const DAYS = Number(process.env.DAYS ?? 30);
const OUT = process.env.OUT;
const FRACTION = process.env.FRACTION === '1';
const MIX = process.env.MIX === '1';
const STEADY = process.env.STEADY === '1';
/** MIX: series i * 4 + window whose index mod 32 is listed are fractional (10 of 64, no credits). */
const MIXED = new Set([1, 6, 13, 20, 26]);
const round4 = (v: number) => Math.round(v * 10_000) / 10_000;
const ACCOUNTS = 16;
const MIN = 60_000;
const HOUR = 3_600_000;
const DAY = 86_400_000;
const START = Date.parse('2026-08-01T00:00:00Z');

function rand(seed: number): number {
  const x = Math.sin(seed) * 10000;
  return x - Math.floor(x);
}

function account(i: number, t: number): any {
  const minute = Math.floor((t - START) / MIN);
  const busy =
    Math.floor(minute / 60) % 24 >= 8 + (i % 6) && Math.floor(minute / 60) % 24 < 14 + (i % 6);
  const fiveStart = Math.floor(minute / 300) * 300;
  let five = Math.min(
    100,
    Math.floor(busy ? (minute - fiveStart) / (3 + (i % 4)) : (fiveStart / 300) % 7)
  );
  const weekStart = Math.floor(minute / 10080) * 10080;
  let week = Math.min(100, Math.floor((minute - weekStart) / (80 + i * 3)));
  if (FRACTION) {
    five = Math.min(100, five + rand(five * 7 + i) * 0.9);
    week = Math.min(100, week + rand(week * 11 + i) * 0.9);
  }
  const windows = [
    {
      key: 'five_hour',
      label: '5 hour',
      usedPercent: five,
      remainingPercent: 100 - five,
      resetAt: new Date(START + (fiveStart + 300) * MIN).toISOString(),
      windowMinutes: 300,
      used: null,
      limit: null,
      unit: null,
    },
    {
      key: 'seven_day',
      label: 'week',
      usedPercent: week,
      remainingPercent: 100 - week,
      resetAt: new Date(START + (weekStart + 10080) * MIN).toISOString(),
      windowMinutes: 10080,
      used: null,
      limit: null,
      unit: null,
    },
    {
      key: 'seven_day_opus',
      label: 'week (Opus)',
      usedPercent: FRACTION ? week / 2 : Math.floor(week / 2),
      remainingPercent: 100 - (FRACTION ? week / 2 : Math.floor(week / 2)),
      resetAt: new Date(START + (weekStart + 10080) * MIN).toISOString(),
      windowMinutes: 10080,
      used: null,
      limit: null,
      unit: null,
    },
    {
      key: 'credits',
      label: 'Credits',
      kind: 'balance' as const,
      usedPercent: null,
      remainingPercent: null,
      resetAt: null,
      windowMinutes: null,
      used: null,
      limit: null,
      remaining: Math.round(5000 - minute / (40 + i)),
      unit: 'credits',
    },
  ];
  if (MIX) {
    // Rising a little every minute through each window, as metered percentages do.
    const rising = [
      (minute - fiveStart) * 0.0731 * (1 + (i % 3)),
      (minute - weekStart) * 0.00217 * (1 + (i % 3)),
      (minute - weekStart) * 0.00131 * (1 + (i % 3)),
    ];
    windows.forEach((w, k) => {
      if (k < 3 && MIXED.has((i * 4 + k) % 32)) {
        const used = round4(Math.min(100, rising[k]));
        w.usedPercent = used;
        w.remainingPercent = round4(100 - used);
      }
    });
  }
  return {
    id: `claude:acct${i}`,
    provider: 'claude',
    providerLabel: 'Claude',
    label: `acct${i}`,
    email: null,
    plan: 'max',
    platform: 'mac',
    source: 'Claude quota on Mac',
    status: rand(minute * 31 + i) < 0.002 ? 'error' : 'ok',
    message: null,
    fetchedAt: new Date(t).toISOString(),
    sampledAt: new Date(t).toISOString(),
    isActive: i === 0,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    windows,
  };
}

function dirBytes(dir: string): number {
  let total = 0;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== 'backups') total += dirBytes(full);
    } else if (entry.name !== 'store.lock') total += fs.statSync(full).size;
  }
  return total;
}

function median(xs: number[]): number {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
}

const moduleFile =
  process.env.MODULE ?? path.resolve(__dirname, '../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: path.resolve(moduleFile) });
if (!loaded.module) throw new Error(`no module at ${moduleFile}: bun run store:build`);
const NativeStore = loaded.module.NativeStore;
if (OUT) fs.mkdirSync(OUT, { recursive: true });
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-size-'));
const open = async (at = dir) => {
  const handle = new NativeStore();
  await handle.call('open', JSON.stringify({ dir: at }));
  return handle;
};

/** The worst case for day values: every window changes every minute, with non-round values. */
async function worstCase() {
  const at = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-worst-'));
  const handle = await open(at);
  const store = new AacStore(handle, at);
  const key = (minute: number, i: number) => {
    const used = 100 * rand(minute * 13 + i * 7);
    return { usedPercent: used, remainingPercent: 100 - used };
  };
  const accountAt = (t: number) => {
    const minute = Math.floor((t - START) / MIN);
    return {
      ...account(0, t),
      status: 'ok',
      windows: ['five_hour', 'seven_day', 'seven_day_opus', 'credits'].map((name, i) => ({
        key: name,
        label: name,
        ...key(minute, i),
        resetAt: new Date(START + 9 * DAY).toISOString(),
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      })),
    };
  };
  let batch: any[] = [];
  for (let minute = 0; minute < 3 * 1440; minute++) {
    const t = START + minute * MIN;
    batch.push(storeObservationFromAccount(accountAt(t), t, 90 * DAY));
    if (batch.length >= 200) {
      await store.record(batch as never);
      batch = [];
    }
  }
  if (batch.length) await store.record(batch as never);
  const identity = (
    batch.length ? batch[0] : storeObservationFromAccount(accountAt(START), START, 90 * DAY)
  ).account.identity;
  const mid = START + DAY + 11 * HOUR;
  const body = JSON.stringify({
    identities: [identity],
    windowKey: 'five_hour',
    fromMs: mid,
    toMs: mid + HOUR,
    resolution: 'raw',
  });
  const samples: number[] = [];
  let answer = '';
  for (let i = 0; i < 300; i++) {
    const t0 = performance.now();
    answer = await handle.call('seriesPoints', body);
    samples.push(performance.now() - t0);
  }
  const counts: any = await store.counts();
  await store.close();
  fs.rmSync(at, { recursive: true, force: true });
  return {
    rawOneHourMedianMs: +median(samples).toFixed(3),
    points: JSON.parse(answer).series[0].points.length,
    spans: counts.reading,
    answerSha256: crypto.createHash('sha256').update(answer).digest('hex'),
  };
}

if (process.env.WORST_ONLY === '1') {
  console.log(JSON.stringify({ worstCase: await worstCase() }, null, 1));
  fs.rmSync(dir, { recursive: true, force: true });
  process.exit(0);
}

let handle = await open();
let store = new AacStore(handle, dir);
const started = Date.now();
const curve: { day: number; redb: number; bytes: number; bytesPerReading: number }[] = [];
let batch: any[] = [];
for (let minute = 0; minute < DAYS * 1440; minute++) {
  const t = START + minute * MIN;
  for (let i = 0; i < ACCOUNTS; i++) {
    const o = storeObservationFromAccount(account(i, t), t, 90 * DAY);
    if (o) batch.push(o);
  }
  if (batch.length >= 1600) {
    await store.record(batch as never);
    batch = [];
  }
  if (STEADY && minute === (DAYS - 1) * 1440) {
    // A real store seals its segment at midnight and compresses it at the next maintenance.
    if (batch.length) await store.record(batch as never);
    batch = [];
    await store.close();
    handle = await open();
    store = new AacStore(handle, dir);
    await store.maintain({ nowMs: t });
  }
  if (minute % 1440 === 1439) {
    await store.maintain({ nowMs: t + 3 * HOUR });
    const reading = ((await store.counts()) as any).reading as number;
    const bytes = dirBytes(dir);
    curve.push({
      day: (minute + 1) / 1440,
      redb: fs.statSync(path.join(dir, 'aac.redb')).size,
      bytes,
      bytesPerReading: Math.round(bytes / reading),
    });
  }
}
if (batch.length) await store.record(batch as never);
const END = START + DAYS * DAY;
await store.maintain({ nowMs: END + 3 * HOUR });
const seconds = Math.round((Date.now() - started) / 1000);
const redbBeforeCompact = fs.statSync(path.join(dir, 'aac.redb')).size;
await store.maintain({ nowMs: END + 3 * HOUR, compact: true });
const redbAfterCompactOpen = fs.statSync(path.join(dir, 'aac.redb')).size;
const counts: any = await store.counts();
const status: any = await store.status();
await store.close();
const storeBytes = dirBytes(dir);
const redbBytes = fs.statSync(path.join(dir, 'aac.redb')).size;
// A real day's segment seals at midnight; this replay's clock does not, so a raw tail remains.
const logFiles = ['readings', 'ledger'].flatMap((log) =>
  fs.readdirSync(path.join(dir, 'log', log)).map((name) => path.join(dir, 'log', log, name))
);
const logSizes = logFiles.map((file) => ({ file, bytes: fs.statSync(file).size }));
const logCompressedBytes = logSizes
  .filter((f) => f.file.endsWith('.zst'))
  .reduce((n, f) => n + f.bytes, 0);
const logRawTailBytes = logSizes
  .filter((f) => /\.(seglog|active)$/.test(f.file))
  .reduce((n, f) => n + f.bytes, 0);

let json: any = null;
for (let t = Math.max(START, END - 30 * DAY); t < END; t += t < END - DAY ? 60 * MIN : MIN) {
  json = appendAccountAnalyticsSnapshot(
    json,
    Array.from({ length: ACCOUNTS }, (_, i) => account(i, t)),
    t
  );
}
const jsonBytes = Buffer.byteLength(JSON.stringify(json));
const jsonReadings = (json?.records ?? []).reduce((n: number, r: any) => n + r.windows.length, 0);

// ---- queries: answers are hashed (and saved) so before and after can be compared byte for byte.
const obs = storeObservationFromAccount(account(3, END - MIN), END - MIN, 90 * DAY) as any;
const allObs = Array.from(
  { length: ACCOUNTS },
  (_, i) => storeObservationFromAccount(account(i, END - MIN), END - MIN, 90 * DAY) as any
);
const id = obs.account.identity as string;
const series = allObs.flatMap((o) =>
  o.windows.map((w: any) => ({ identity: o.account.identity, window: w.identity }))
);
const mid = START + Math.floor(DAYS / 2) * DAY;
const queries: [string, string, unknown, number][] = [
  [
    'raw 1 hour, 1 series',
    'seriesPoints',
    {
      identities: [id],
      windowKey: 'five_hour',
      fromMs: mid + 11 * HOUR,
      toMs: mid + 12 * HOUR,
      resolution: 'raw',
    },
    200,
  ],
  [
    'raw 1 hour, 1 account (all windows)',
    'seriesPoints',
    { identities: [id], fromMs: mid + 11 * HOUR, toMs: mid + 12 * HOUR, resolution: 'raw' },
    200,
  ],
  [
    'raw 1 day, 1 series',
    'seriesPoints',
    { identities: [id], windowKey: 'five_hour', fromMs: mid, toMs: mid + DAY, resolution: 'raw' },
    100,
  ],
  [
    'raw 30 days, 1 series',
    'seriesPoints',
    { identities: [id], windowKey: 'five_hour', fromMs: START, toMs: END, resolution: 'raw' },
    30,
  ],
  [
    'raw 1 hour, all series',
    'seriesPoints',
    { fromMs: mid + 11 * HOUR, toMs: mid + 12 * HOUR, resolution: 'raw' },
    50,
  ],
  [
    'raw 30 days, all series (20k cap)',
    'seriesPoints',
    { fromMs: START, toMs: END, resolution: 'raw' },
    10,
  ],
  [
    'hourly 1 day, 1 series',
    'seriesPoints',
    { identities: [id], windowKey: 'five_hour', fromMs: mid, toMs: mid + DAY, resolution: 'hour' },
    100,
  ],
  [
    'hourly 30 days, all series',
    'seriesPoints',
    { fromMs: START, toMs: END, resolution: 'hour' },
    10,
  ],
  [
    'daily 30 days, all series',
    'seriesPoints',
    { fromMs: START, toMs: END, resolution: 'day' },
    30,
  ],
  ['all series at time T', 'usageAt', { t: mid + 11 * HOUR + 7 * MIN }, 50],
  [
    'history 24h (analytics thinning)',
    'historyRecords',
    { fromMs: END - DAY, toMs: END, nowMs: END, thinning: { kind: 'history' } },
    10,
  ],
  [
    'history 31d (analytics thinning)',
    'historyRecords',
    { fromMs: END - 31 * DAY, toMs: END, nowMs: END, thinning: { kind: 'history' } },
    5,
  ],
  [
    'history 1h, 1 account, unthinned',
    'historyRecords',
    {
      fromMs: mid + 11 * HOUR,
      toMs: mid + 12 * HOUR,
      nowMs: END,
      thinning: { kind: 'none' },
      identities: [id],
    },
    50,
  ],
  [
    'burn 7d/30d/90d, all series',
    'burnLong',
    { series, spansMs: [7 * DAY, 30 * DAY, 90 * DAY], nowMs: END },
    20,
  ],
  ['periods 30 days, all series', 'periods', { series, fromMs: START, toMs: END }, 20],
  ['oldest reading', 'oldest', {}, 100],
];
const runQueries = async (h: any, time: boolean) => {
  const answers: Record<string, string> = {};
  const timings: Record<
    string,
    { medianMs: number; firstMs: number; runs: number; bytes: number }
  > = {};
  for (const [name, method, args, runs] of queries) {
    const body = JSON.stringify(args);
    const samples: number[] = [];
    let answer = '';
    for (let i = 0; i < (time ? runs : 1); i++) {
      const t0 = performance.now();
      answer = await h.call(method, body);
      samples.push(performance.now() - t0);
    }
    answers[name] = crypto.createHash('sha256').update(answer).digest('hex');
    timings[name] = {
      medianMs: +median(samples).toFixed(3),
      firstMs: +samples[0].toFixed(3),
      runs: samples.length,
      bytes: answer.length,
    };
    if (time && OUT)
      fs.writeFileSync(path.join(OUT, `${name.replace(/[^a-z0-9]+/gi, '_')}.json`), answer);
  }
  return { answers, timings };
};

let t0 = performance.now();
handle = await open();
const reopenMs = Math.round(performance.now() - t0);
const q = await runQueries(handle, true);
await handle.call('close', '{}');

// ---- a rebuild from the logs alone: the same answers.
fs.rmSync(path.join(dir, 'aac.redb'));
t0 = performance.now();
handle = await open();
const rebuildMs = Math.round(performance.now() - t0);
const rebuilt = await runQueries(handle, false);
const differsAfterRebuild = Object.keys(q.answers).filter(
  (k) => q.answers[k] !== rebuilt.answers[k]
);

// ---- write cost: one sampling tick (16 accounts) late in a busy day.
store = new AacStore(handle, dir);
const ticks: number[] = [];
const tickStart = END + 11 * HOUR;
for (let minute = 0; minute < 120; minute++) {
  const t = tickStart + minute * MIN;
  const tick = Array.from({ length: ACCOUNTS }, (_, i) =>
    storeObservationFromAccount(account(i, t), t, 90 * DAY)
  );
  const t1 = performance.now();
  await store.record(tick as never);
  ticks.push(performance.now() - t1);
}
await store.close();
fs.rmSync(dir, { recursive: true, force: true });

const results = {
  worstCase: await worstCase(),
  curve,
  days: DAYS,
  fraction: FRACTION,
  mix: MIX,
  steady: STEADY,
  accounts: ACCOUNTS,
  windowsPerAccount: 4,
  seconds,
  counts,
  redbStats: status.redbStats,
  redbBeforeCompact,
  redbAfterCompactOpen,
  redbBytes,
  logBytesOnDisk: status.logs.readings.bytesOnDisk + status.logs.ledger.bytesOnDisk,
  readingsLogRawBytes: status.logs.readings.sealedRawBytes,
  storeBytes,
  storeBytesPerReading: Math.round(storeBytes / counts.reading),
  jsonBytes,
  jsonRecords: json?.records.length,
  jsonWindowReadings: jsonReadings,
  jsonBytesPerWindowReading: Math.round(jsonBytes / jsonReadings),
  logCompressedBytes,
  logRawTailBytes,
  reopenMs,
  rebuildMs,
  differsAfterRebuild,
  tickMedianMs: +median(ticks).toFixed(2),
  tickMaxMs: +Math.max(...ticks).toFixed(2),
  answers: q.answers,
  timings: q.timings,
};
if (OUT) fs.writeFileSync(path.join(OUT, 'results.json'), JSON.stringify(results, null, 2));
console.log(
  JSON.stringify({ ...results, curve: undefined, answers: undefined, timings: undefined }, null, 1)
);
process.exit(0);
