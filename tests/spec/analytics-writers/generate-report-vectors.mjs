// G14 report-builder parity vectors: the TS answers (src/store/activity-reports.ts) the Rust port
// in crates/aac-store/src/report.rs reproduces. Synthetic worker results and source states only —
// no real logs, roots or credentials. Run: bun tests/spec/analytics-writers/generate-report-vectors.mjs
// (--check regenerates and compares without writing).
import { writeFileSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { buildActivityReports, rowsReport } from '../../../src/store/activity-reports.ts';

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, 'report-vectors.json');

const NOW = Date.parse('2026-11-15T12:00:00Z');
const WINDOW_FROM = NOW - 31 * 86_400_000;
const HOUR = 3_600_000;
const iso = (ms) => new Date(ms).toISOString();
const hourLabel = (ms) => {
  const d = new Date(Math.floor(ms / HOUR) * HOUR);
  return `${d.toISOString().slice(0, 10)} ${d.toISOString().slice(11, 13)}:00`;
};

// A priced cell as the collector hands it (already costed; the report builder never prices).
const cell = (hourMs, over = {}) => ({
  hour: iso(Math.floor(hourMs / HOUR) * HOUR),
  sessionKey: null,
  route: 'anthropic',
  modelRaw: 'claude-sonnet-4-5',
  inputTokens: 1000,
  outputTokens: 200,
  cacheCreationTokens: 50,
  cacheReadTokens: 300,
  costUsd: 0.0123,
  fallbackCostUsd: 0,
  events: 4,
  ...over,
});

const breakdown = (over = {}) => ({
  modelName: 'claude-sonnet-4-5',
  provider: 'anthropic',
  inputTokens: 1000,
  outputTokens: 200,
  cacheCreationTokens: 50,
  cacheReadTokens: 300,
  cost: 0.02,
  requestCount: 4,
  ...over,
});

const hourly = (hourMs, over = {}) => ({
  hour: hourLabel(hourMs),
  requestCount: 4,
  modelBreakdowns: [breakdown()],
  ...over,
});

const sessionRow = (key, over = {}) => ({
  sessionId: key,
  firstActivity: iso(NOW - 2 * HOUR),
  lastActivity: iso(NOW - HOUR),
  target: 'claude',
  modelBreakdowns: [breakdown()],
  ...over,
});

const worker = (over = {}) => ({ hourly: [], session: [], ...over });
const source = (tool, host, state, over = {}) => ({ tool, host, state, ...over });

const H1 = NOW - HOUR;
const H2 = NOW - 2 * HOUR;
const KEY_A = '0123456789abcdef';
const KEY_B = 'fedcba9876543210';

// Each pass: the buildActivityReports inputs plus the labels the Rust test reads.
const passes = [
  {
    name: 'local-ok-complete',
    local: { claude: [worker({ cells: [cell(H1), cell(H2)] })] },
    remote: [],
    states: [source('claude', 'ubuntu', 'ok', { lastScanAt: iso(NOW - 60_000) })],
    localComplete: ['claude'],
    scannerOwned: [],
  },
  {
    name: 'local-hourly-derived-partial',
    local: { claude: [worker({ hourly: [hourly(H1), hourly(H2)] })] },
    remote: [],
    states: [source('claude', 'ubuntu', 'ok', { lastScanAt: iso(NOW) })],
    localComplete: ['claude'],
    scannerOwned: [],
  },
  {
    name: 'local-incomplete-ok-becomes-partial',
    local: { codex: [worker({ cells: [cell(H1)] })] },
    remote: [],
    states: [source('codex', 'ubuntu', 'ok')],
    localComplete: [],
    scannerOwned: [],
  },
  {
    name: 'scanner-owned-ubuntu-complete',
    local: { codex: [worker({ cells: [cell(H2)] })] },
    remote: [{ tool: 'codex', host: 'ubuntu', data: worker({ cells: [cell(H1)] }) }],
    states: [source('codex', 'ubuntu', 'ok', { lastScanAt: iso(NOW) })],
    localComplete: [],
    scannerOwned: ['codex'],
  },
  {
    name: 'scanner-only-cursor-empty',
    local: {},
    remote: [],
    states: [source('cursor', 'ubuntu', 'no_usage')],
    localComplete: [],
    scannerOwned: [],
  },
  {
    name: 'remote-mac-ok',
    local: {},
    remote: [{ tool: 'claude', host: 'mac', data: worker({ cells: [cell(H1)], host: 'mac' }) }],
    states: [source('claude', 'mac', 'ok', { lastScanAt: iso(NOW - 30_000) })],
    localComplete: [],
    scannerOwned: [],
  },
  {
    name: 'future-dated-counted',
    local: {
      claude: [
        worker({
          cells: [cell(H1), cell(NOW + 10 * 60_000, { events: 7 })],
          futureDated: 2,
        }),
      ],
    },
    remote: [],
    states: [source('claude', 'ubuntu', 'ok', { lastScanAt: iso(NOW) })],
    localComplete: ['claude'],
    scannerOwned: [],
  },
  {
    name: 'sessions-dedupe-and-target',
    local: {
      claude: [
        worker({
          cells: [cell(H1)],
          session: [
            sessionRow(KEY_A),
            sessionRow(KEY_A, {
              modelBreakdowns: [breakdown({ inputTokens: 9000 })],
            }),
            sessionRow(KEY_B, { target: 'Bad Target!' }),
            sessionRow('not-a-key'),
            sessionRow(KEY_B, {
              firstActivity: iso(NOW + HOUR),
              lastActivity: iso(NOW + 2 * HOUR),
            }),
          ],
        }),
      ],
    },
    remote: [],
    states: [source('claude', 'ubuntu', 'ok', { lastScanAt: iso(NOW) })],
    localComplete: ['claude'],
    scannerOwned: [],
  },
  {
    name: 'states-grid',
    local: { claude: [worker({ cells: [cell(H1)] })] },
    remote: [],
    states: [
      source('claude', 'ubuntu', 'cached', { cause: 'mirror_election', lastScanAt: iso(NOW) }),
      source('claude', 'mac', 'unavailable', { detail: 'SSH timed out after 30s' }),
      source('claude', 'windows', 'unavailable', { detail: 'no local usage log found' }),
      source('codex', 'ubuntu', 'not_installed'),
      source('codex', 'mac', 'scan_failed', { cause: 'reader_crash' }),
      source('gemini', 'ubuntu', 'scanning'),
      source('gemini', 'mac', 'installed_no_logs'),
      source('zcode', 'ubuntu', 'ok', { lastScanAt: iso(NOW) }),
    ],
    localComplete: ['claude', 'zcode'],
    scannerOwned: [],
  },
  {
    name: 'no-usage-empty',
    local: { claude: [worker()] },
    remote: [],
    states: [source('claude', 'ubuntu', 'no_usage', { lastScanAt: iso(NOW) })],
    localComplete: ['claude'],
    scannerOwned: [],
  },
  {
    name: 'unavailable-with-cells-partial',
    local: { claude: [worker({ cells: [cell(H1)] })] },
    remote: [],
    states: [source('claude', 'ubuntu', 'unavailable', { detail: 'partial read' })],
    localComplete: [],
    scannerOwned: [],
  },
];

const passVectors = passes.map((p) => {
  const local = new Map(Object.entries(p.local));
  const reports = buildActivityReports({
    local,
    remote: p.remote,
    states: p.states,
    windowFromMs: WINDOW_FROM,
    now: NOW,
    localComplete: (tool) => p.localComplete.includes(tool),
    scannerOwned: new Set(p.scannerOwned),
  });
  return {
    name: p.name,
    input: {
      local: p.local,
      remote: p.remote,
      states: p.states,
      windowFromMs: WINDOW_FROM,
      now: NOW,
      localComplete: p.localComplete,
      scannerOwned: p.scannerOwned,
    },
    expected: reports,
  };
});

const rowsCases = [
  {
    name: 'backfill-complete',
    args: {
      computer: 'ubuntu',
      tool: 'claude',
      data: [worker({ cells: [cell(H1), cell(H2)], session: [sessionRow(KEY_A)] })],
      kind: 'backfill',
      readAt: NOW - 60_000,
      complete: true,
      now: NOW,
    },
  },
  {
    name: 'import-partial',
    args: {
      computer: 'mac',
      tool: 'codex',
      data: [worker({ hourly: [hourly(H1)] })],
      kind: 'import',
      readAt: NOW - 60_000,
      complete: true,
      cause: 'old_cache',
      now: NOW,
    },
  },
  {
    name: 'backfill-empty-no-usage',
    args: {
      computer: 'ubuntu',
      tool: 'gemini',
      data: [worker()],
      kind: 'backfill',
      readAt: NOW,
      complete: true,
      now: NOW,
    },
  },
  {
    name: 'import-incomplete-scanning',
    args: {
      computer: 'ubuntu',
      tool: 'claude',
      data: [worker()],
      kind: 'import',
      readAt: NOW,
      complete: false,
      now: NOW,
    },
  },
  {
    name: 'backfill-future-counted',
    args: {
      computer: 'ubuntu',
      tool: 'claude',
      data: [worker({ cells: [cell(NOW + HOUR, { events: 3 })], futureDated: 1 })],
      kind: 'backfill',
      readAt: NOW,
      complete: true,
      now: NOW,
    },
  },
];

const rowsVectors = rowsCases.map((c) => ({
  name: c.name,
  input: c.args,
  expected: rowsReport(c.args),
}));

const vectors = {
  note: 'G14 report-builder parity: TS answers for the Rust port (synthetic rows only).',
  now: NOW,
  passes: passVectors,
  rows: rowsVectors,
};

const json = `${JSON.stringify(vectors, null, 2)}\n`;
if (process.argv.includes('--check')) {
  const before = readFileSync(out, 'utf8');
  if (before !== json) {
    console.error('report-vectors.json is stale; regenerate without --check');
    process.exit(1);
  }
  console.log('report-vectors.json up to date');
} else {
  writeFileSync(out, json);
  console.log(`wrote ${out}: ${passVectors.length} passes, ${rowsVectors.length} rows reports`);
}