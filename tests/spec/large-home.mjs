/**
 * L1 measurement: a large synthetic AAC home, deterministic and fixture-only (no real account,
 * credential or path). The M3 design measures every route group on it under Node:
 *   - 50 Claude desktop profiles, 30 hidden accounts, preferences and the refresh setting (the
 *     private files the profile and account reads answer from);
 *   - 200 agent reports (thread, tasks and card documents of 50 writers) and 5,000 finished runs
 *     (run@1), in the share layout `<share>/v1/writers/<writer>/...` the Status and Scoreboard
 *     readers scan;
 *   - 30 accounts x 12 windows x 90 days of Analytics observations, as the store's `record`
 *     inputs (one JSON line per observation; R3/R5 ingest them), every 6 hours by default;
 *   - R1: the same 30 accounts x 12 windows as dashboard rows (`dashboard-accounts.json`), which the
 *     measurement server feeds to the real dashboard service for Home.
 *
 *   node tests/spec/large-home.mjs <dir> [--profiles 50] [--reports 200] [--runs 5000]
 *        [--accounts 30] [--windows 12] [--days 90] [--cadence-hours 6]
 * Writes <dir>/.ccs, <dir>/share, <dir>/analytics/observations.jsonl and <dir>/dashboard-accounts.json, and
 * prints a summary.
 */
import fs from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const target = args.find((a) => !a.startsWith('--') && !/^\d+$/.test(a));
if (!target) {
  console.error('usage: node tests/spec/large-home.mjs <dir> [--runs N ...]');
  process.exit(2);
}
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? Number(args[i + 1]) : fallback;
};
const N = {
  profiles: option('profiles', 50),
  reports: option('reports', 200),
  runs: option('runs', 5000),
  accounts: option('accounts', 30),
  windows: option('windows', 12),
  days: option('days', 90),
  cadenceHours: option('cadence-hours', 6),
};
const NOW = Date.parse('2026-11-06T16:30:00.000Z');
const HOUR = 3_600_000;
const iso = (ms) => new Date(ms).toISOString();

// A small seeded generator, so the same arguments always write the same home.
let seed = 0x5eed1234;
const random = () => {
  seed = (seed * 1664525 + 1013904223) >>> 0;
  return seed / 2 ** 32;
};
const pick = (list) => list[Math.floor(random() * list.length)];

function write(file, value, mode = 0o600) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, typeof value === 'string' ? value : JSON.stringify(value, null, 2) + '\n', {
    mode,
  });
}

const root = path.resolve(target);
const ccs = path.join(root, '.ccs');
const share = path.join(root, 'share');
fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });

// Private files of the profile and account reads.
write(path.join(ccs, 'claude-desktop-profiles.json'), {
  version: 1,
  profiles: Array.from({ length: N.profiles }, (_, i) => ({
    id: `profile_${i}`,
    email: `user${i}@example.com`,
    mac: {
      launcherName: `Claude ${i}`,
      launcherPath: `/Applications/Claude ${i}.app`,
      profilePath: `/Users/fixture/Library/Application Support/Claude-${i}`,
      sshHost: 'mac',
      ...(i === 0 ? { isDefault: true } : {}),
    },
    windows: {
      launcherName: `Claude ${i}`,
      profilePath: `C:\\Users\\fixture\\AppData\\Roaming\\Claude-${i}`,
      sshHost: 'pc.lan',
    },
  })),
});
const accounts = Array.from({ length: N.accounts }, (_, i) => ({
  provider: pick(['claude', 'codex', 'cursor', 'antigravity']),
  id: i,
}));
write(path.join(ccs, 'account-visibility.json'), {
  version: 1,
  hiddenProviders: ['muse'],
  hiddenAccountIds: accounts.map((a) => `${a.provider}:account-${a.id}`),
  trayHiddenProviders: [],
  trayHiddenAccountIds: accounts.slice(0, 5).map((a) => `${a.provider}:account-${a.id}`),
});
write(path.join(ccs, 'dashboard-preferences.json'), {
  timeZone: 'America/New_York',
  snapshotCleanup: { auto: false },
  usageLogSources: [],
});
write(path.join(ccs, 'account-refresh-settings.json'), { refreshIntervalSeconds: 60 });

// Agent reports: per writer a thread document, a task board, and cards.
const writers = Array.from({ length: Math.max(1, Math.ceil(N.reports / 4)) }, (_, i) =>
  `${pick(['vm', 'mac', 'nas1'])}.agent-${i}`
);
let reports = 0;
const report = (writer, rel, doc) => {
  write(path.join(share, 'v1', 'writers', writer, rel), doc, 0o644);
  reports++;
};
for (const [w, writer] of writers.entries()) {
  if (reports >= N.reports) break;
  const at = (ms) => iso(NOW - ms);
  report(writer, 'thread.json', {
    schema: 'agent-reports/thread@1',
    writer,
    seq: 1 + w,
    writtenAt: at(60_000 * (1 + (w % 30))),
    ttlSeconds: 1800,
    agent: { name: writer.split('.')[1], model: 'claude-opus-5-5', effort: 'high' },
    project: `proj-${w % 6}`,
    stream: `S${w % 4}`,
    state: pick(['working', 'waiting', 'done']),
    doing: `Task ${w}`,
    blockedOn: null,
    progress: null,
    links: [],
  });
  if (reports >= N.reports) break;
  report(writer, 'tasks.json', {
    schema: 'agent-reports/tasks@1',
    writer,
    seq: 1,
    writtenAt: at(120_000),
    project: `proj-${w % 6}`,
    projectLabel: `Project ${w % 6}`,
    stream: `S${w % 4}`,
    streamLabel: `Stream ${w % 4}`,
    owner: 'lead',
    goal: 'Ship it',
    board: null,
    items: Array.from({ length: 8 }, (_, k) => ({
      id: `t${k}`,
      title: `Item ${k}`,
      state: pick(['todo', 'doing', 'done']),
      updatedAt: at(600_000 + k * 60_000),
    })),
  });
  for (let c = 0; c < 2 && reports < N.reports; c++) {
    const id = `${writer}--q${c}`;
    report(writer, `cards/${id}.json`, {
      schema: 'agent-reports/card@1',
      id,
      writer,
      seq: 1,
      writtenAt: at(60_000),
      project: `proj-${w % 6}`,
      stream: `S${w % 4}`,
      askedBy: writer.split('.')[1],
      replyTo: { pmMail: writer.split('.')[1], t3Thread: null },
      state: 'open',
      label: `S-q${c}`,
      title: `Question ${c}`,
      now: 'Plain words.',
      nowAsOf: null,
      oldAnswer: null,
      whatChanged: null,
      options: [
        { key: 'A', label: 'First', gets: 'Simple', costs: null, media: [], sections: [] },
        { key: 'B', label: 'Second', gets: null, costs: 'More work', media: [], sections: [] },
      ],
      recommendation: { key: 'A', why: 'Simpler.' },
      multiple: false,
      urgency: 'today',
      blocks: null,
      batch: null,
      evidence: [],
      replies: [],
      askedAt: at(600_000),
    });
  }
}

// Finished runs, spread over the last 30 days.
const models = [
  ['anthropic', 'claude-opus-5-5', 'claude-code'],
  ['anthropic', 'claude-sonnet-5-5', 'claude-code'],
  ['openai', 'gpt-6.1-sol', 'codex'],
];
for (let i = 0; i < N.runs; i++) {
  const writer = writers[i % writers.length];
  const ended = NOW - Math.floor(random() * 30 * 24) * HOUR;
  const [provider, id, harness] = pick(models);
  const local = `run-${i}`;
  write(
    path.join(share, 'v1', 'writers', writer, 'runs', iso(ended).slice(0, 10), `${writer}--${local}.json`),
    JSON.stringify({
      schema: 'agent-reports/run@1',
      id: `${writer}--${local}`,
      writer,
      seq: 1,
      writtenAt: iso(ended),
      source: 'observed',
      project: `proj-${i % 6}`,
      stream: null,
      task: { id: local, type: pick(['build', 'fix', 'review', 'test']), title: `Task ${i}`, size: { tasks: 1 } },
      model: { provider, id, harness, effort: pick(['high', 'max']) },
      startedAt: null,
      endedAt: iso(ended),
      wallSeconds: 60 + Math.floor(random() * 3600),
      outcome: pick(['done', 'done', 'done', 'failed']),
    }),
    0o644
  );
}

// Analytics observations: the store's `record` inputs.
const lines = [];
const start = NOW - N.days * 24 * HOUR;
const windows = Array.from({ length: N.windows }, (_, k) => ({
  key: k === 0 ? 'five_hour' : k === 1 ? 'seven_day' : `window_${k}`,
  label: k === 0 ? '5 hour' : k === 1 ? '7 day' : `Window ${k}`,
}));
for (let t = start; t <= NOW; t += N.cadenceHours * HOUR)
  for (const account of accounts) {
    const used = (k) => Math.round(((t / HOUR + account.id * 7 + k * 3) % 100) * 100) / 100;
    lines.push(
      JSON.stringify({
        account: {
          identity: account.id.toString(16).padStart(64, '0'),
          provider: account.provider,
          account_id: `${account.provider}:account-${account.id}`,
          plan: null,
          platform: 'ubuntu',
          source: 'large-home',
          status: 'ok',
          is_active: account.id === 0,
        },
        sampled_at_ms: t,
        observed_at_ms: t,
        import_ref: null,
        windows: windows.map((w, k) => ({
          identity: JSON.stringify([w.key, null, null]),
          key: w.key,
          label: w.label,
          used_pct: used(k),
          remaining_pct: Math.round((100 - used(k)) * 100) / 100,
          reset_at_ms: t + (k + 1) * HOUR,
        })),
      })
    );
  }
write(path.join(root, 'analytics', 'observations.jsonl'), lines.join('\n') + '\n');

// R1: the dashboard's account rows (30 accounts x 12 windows by default), as the additional-accounts collector
// publishes them (provider ids from the table's additional providers). The measurement server feeds them to the
// real AccountDashboardService through that collector's snapshot, so Home's measurements run on this size.
const DASHBOARD_PROVIDERS = ['muse', 'cursor', 'kimi-code', 'qwen', 'zai', 'opencode-go'];
const dashboardAccounts = Array.from({ length: N.accounts }, (_, i) => {
  const provider = DASHBOARD_PROVIDERS[i % DASHBOARD_PROVIDERS.length];
  const sampled = NOW - (i % 7) * 60_000;
  return {
    id: `${provider}:fixture-${i}`,
    provider,
    providerLabel: provider,
    label: `Fixture ${i}`,
    email: `user${i}@example.com`,
    plan: pick(['pro', 'plus', 'team', 'free']),
    status: 'ok',
    platform: 'ubuntu',
    source: 'Fixture usage',
    fetchedAt: iso(sampled),
    sampledAt: iso(sampled),
    windows: Array.from({ length: N.windows }, (_, k) => ({
      key: `window-${k}`,
      label: `Window ${k}`,
      kind: 'rate_limit',
      usedPercent: Math.round(random() * 10000) / 100,
      windowMinutes: [300, 10080, 43200][k % 3],
      // Some resets have passed since the reading (markPassedResets marks them per response).
      resetAt: iso(sampled + (k - 2) * HOUR),
    })),
  };
});
write(path.join(root, 'dashboard-accounts.json'), dashboardAccounts);

console.log(
  JSON.stringify({
    root,
    profiles: N.profiles,
    reports,
    writers: writers.length,
    runs: N.runs,
    accounts: N.accounts,
    windows: N.windows,
    days: N.days,
    observations: lines.length,
  })
);
