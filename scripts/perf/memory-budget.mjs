#!/usr/bin/env node
/**
 * Phase 4 memory budget test runner for the shipped AAC dashboard server.
 *
 * Boots dist/native/ai-account-center against a deterministic real-sized synthetic
 * fixture in a completely isolated temporary home (never touching real ~/.ccs or
 * the live share), signs in via loopback setup with a made-up login, polls the
 * dashboard routes on their real page timers for 5 minutes, samples server and
 * child RSS/PSS from /proc every 2 seconds, writes a JSON summary, and exits
 * non-zero if any limit in config/memory-budget.json is exceeded.
 *
 * Usage:
 *   node scripts/perf/memory-budget.mjs
 *     [--duration 300] [--steady-after 120] [--sample-interval 2]
 *     [--seed 20261125] [--config config/memory-budget.json]
 *     [--out <summary.json>] [--fixture-dir <dir>] [--keep] [--no-check] [--json]
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { DEFAULT_OPTIONS, generateMemoryFixture } from './memory-fixture.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..', '..');
const DEFAULT_CONFIG_PATH = path.join(REPO_ROOT, 'config', 'memory-budget.json');
const KIB_PER_MIB = 1024;

/** Default polling plan matching open dashboard pages + both desktop Home trays. */
export const POLL_PLAN = Object.freeze([
  { name: 'status', path: '/api/status', periodSec: 10, offsetSec: 0 },
  { name: 'scoreboard', path: '/api/scoreboard', periodSec: 30, offsetSec: 1 },
  {
    name: 'analytics7d',
    path: '/api/accounts/analytics?platform=mac&range=7d&provider=all&account=all',
    periodSec: 60,
    offsetSec: 2,
  },
  {
    name: 'analytics30d',
    path: '/api/accounts/analytics?platform=mac&range=30d&provider=all&account=all',
    periodSec: 300,
    offsetSec: 3,
  },
  {
    name: 'home-mac',
    path: '/api/views/home?platform=mac&refresh=false',
    periodSec: 60,
    offsetSec: 4,
  },
  {
    name: 'home-win',
    path: '/api/views/home?platform=windows&refresh=false',
    periodSec: 60,
    offsetSec: 5,
  },
  {
    name: 'accounts',
    path: '/api/accounts/dashboard?platform=mac&refresh=false',
    periodSec: 60,
    offsetSec: 6,
  },
]);

const FIXTURE_USER = 'perfadmin';
const FIXTURE_PASS = 'Perf-Correct-Horse-Battery-9';

/** Parse /proc/<pid>/smaps_rollup text into KiB counters. */
export function parseSmapsRollup(text) {
  const out = { rssKiB: 0, pssKiB: 0, pssAnonKiB: 0, pssFileKiB: 0, swapKiB: 0 };
  for (const line of text.split('\n')) {
    const colon = line.indexOf(':');
    if (colon < 0) continue;
    const key = line.slice(0, colon).trim();
    const val = Number.parseInt(line.slice(colon + 1).trim(), 10);
    if (!Number.isFinite(val)) continue;
    if (key === 'Rss') out.rssKiB = val;
    else if (key === 'Pss') out.pssKiB = val;
    else if (key === 'Pss_Anon') out.pssAnonKiB = val;
    else if (key === 'Pss_File') out.pssFileKiB = val;
    else if (key === 'Swap') out.swapKiB = val;
  }
  return out;
}

/** Fallback parser for /proc/<pid>/status when smaps_rollup is unreadable. */
export function parseProcStatusRss(text) {
  let rssKiB = 0;
  let swapKiB = 0;
  for (const line of text.split('\n')) {
    if (line.startsWith('VmRSS:')) rssKiB = Number.parseInt(line.slice(6).trim(), 10) || 0;
    else if (line.startsWith('VmSwap:')) swapKiB = Number.parseInt(line.slice(7).trim(), 10) || 0;
  }
  return { rssKiB, pssKiB: rssKiB, pssAnonKiB: 0, pssFileKiB: 0, swapKiB };
}

/** Parse /proc/<pid>/stat text for parent PID and thread count. */
export function parseProcStat(text) {
  const close = text.lastIndexOf(')');
  if (close < 0) return null;
  const fields = text.slice(close + 2).trim().split(/\s+/);
  if (fields.length < 18) return null;
  const ppid = Number.parseInt(fields[1], 10);
  const threads = Number.parseInt(fields[17], 10);
  if (!Number.isFinite(ppid)) return null;
  return { ppid, threads: Number.isFinite(threads) ? threads : 1 };
}

/** Walk a pid -> children map from rootPid and return every descendant PID. */
export function collectDescendants(rootPid, parentByPid) {
  const kidsByParent = new Map();
  for (const [pid, ppid] of parentByPid.entries()) {
    if (!kidsByParent.has(ppid)) kidsByParent.set(ppid, []);
    kidsByParent.get(ppid).push(pid);
  }
  const found = [];
  const stack = [rootPid];
  const visited = new Set([rootPid]);
  while (stack.length > 0) {
    const current = stack.pop();
    for (const child of kidsByParent.get(current) ?? []) {
      if (visited.has(child)) continue;
      visited.add(child);
      found.push(child);
      stack.push(child);
    }
  }
  return found;
}

function readProcMemory(pid) {
  try {
    return parseSmapsRollup(fs.readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8'));
  } catch {
    try {
      return parseProcStatusRss(fs.readFileSync(`/proc/${pid}/status`, 'utf8'));
    } catch {
      return null;
    }
  }
}

function listProcParents() {
  const parentByPid = new Map();
  let entries;
  try {
    entries = fs.readdirSync('/proc');
  } catch {
    return parentByPid;
  }
  for (const name of entries) {
    if (!/^\d+$/.test(name)) continue;
    const pid = Number.parseInt(name, 10);
    try {
      const parsed = parseProcStat(fs.readFileSync(`/proc/${pid}/stat`, 'utf8'));
      if (parsed) parentByPid.set(pid, parsed.ppid);
    } catch {
      // Process exited during scan.
    }
  }
  return parentByPid;
}

/** Sample server process and all descendant processes from /proc. */
export function sampleProcessTree(rootPid, elapsedSec) {
  let rootStat;
  try {
    rootStat = parseProcStat(fs.readFileSync(`/proc/${rootPid}/stat`, 'utf8'));
  } catch {
    return null;
  }
  const server = readProcMemory(rootPid);
  if (!server) return null;
  const parentByPid = listProcParents();
  const kids = collectDescendants(rootPid, parentByPid);
  let childrenRssKiB = 0;
  let childrenPssKiB = 0;
  for (const kid of kids) {
    const mem = readProcMemory(kid);
    if (!mem) continue;
    childrenRssKiB += mem.rssKiB;
    childrenPssKiB += mem.pssKiB;
  }
  return {
    tSec: +elapsedSec.toFixed(2),
    serverRssKiB: server.rssKiB,
    serverPssKiB: server.pssKiB,
    serverAnonKiB: server.pssAnonKiB,
    serverFileKiB: server.pssFileKiB,
    serverSwapKiB: server.swapKiB,
    threads: rootStat?.threads ?? 1,
    childrenCount: kids.length,
    childrenRssKiB,
    childrenPssKiB,
    totalRssKiB: server.rssKiB + childrenRssKiB,
    totalPssKiB: server.pssKiB + childrenPssKiB,
  };
}

export function median(values) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

export function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.floor(sorted.length * p));
  return sorted[idx];
}

function toMiB(kib) {
  return +(kib / KIB_PER_MIB).toFixed(2);
}

/** Compute steady median (after steadyAfterSec), peak, and children peak summary. */
export function summarizeSamples(samples, steadyAfterSec = 120) {
  if (!samples.length) {
    throw new Error('No memory samples were collected from /proc.');
  }
  const steadyWindow = samples.filter((s) => s.tSec >= steadyAfterSec);
  const steadySource = steadyWindow.length > 0 ? steadyWindow : samples;

  const peakServerRssKiB = Math.max(...samples.map((s) => s.serverRssKiB));
  const peakServerPssKiB = Math.max(...samples.map((s) => s.serverPssKiB));
  const childrenPeakRssKiB = Math.max(...samples.map((s) => s.childrenRssKiB));
  const childrenPeakPssKiB = Math.max(...samples.map((s) => s.childrenPssKiB));
  const totalPeakRssKiB = Math.max(...samples.map((s) => s.totalRssKiB));
  const totalPeakPssKiB = Math.max(...samples.map((s) => s.totalPssKiB));

  const steadyMedianRssKiB = median(steadySource.map((s) => s.serverRssKiB));
  const steadyMedianPssKiB = median(steadySource.map((s) => s.serverPssKiB));
  const steadyMedianTotalRssKiB = median(steadySource.map((s) => s.totalRssKiB));
  const steadyMedianTotalPssKiB = median(steadySource.map((s) => s.totalPssKiB));

  return {
    sampleCount: samples.length,
    steadyAfterSec,
    steadySampleCount: steadyWindow.length,
    steadyUsedFallback: steadyWindow.length === 0,
    steadyMedianRssMiB: toMiB(steadyMedianRssKiB),
    steadyMedianPssMiB: toMiB(steadyMedianPssKiB),
    peakRssMiB: toMiB(peakServerRssKiB),
    peakPssMiB: toMiB(peakServerPssKiB),
    childrenPeakRssMiB: toMiB(childrenPeakRssKiB),
    childrenPeakPssMiB: toMiB(childrenPeakPssKiB),
    totalSteadyMedianRssMiB: toMiB(steadyMedianTotalRssKiB),
    totalSteadyMedianPssMiB: toMiB(steadyMedianTotalPssKiB),
    totalPeakRssMiB: toMiB(totalPeakRssKiB),
    totalPeakPssMiB: toMiB(totalPeakPssKiB),
    endRssMiB: toMiB(samples[samples.length - 1].serverRssKiB),
    endPssMiB: toMiB(samples[samples.length - 1].serverPssKiB),
    maxThreads: Math.max(...samples.map((s) => s.threads)),
    maxChildren: Math.max(...samples.map((s) => s.childrenCount)),
  };
}

/** Check measured metrics against config/memory-budget.json limits. */
export function evaluateThresholds(memorySummary, config) {
  const limits = config?.limits ?? {};
  const checks = [
    ['steadyRssMiB', 'steadyMedianRssMiB'],
    ['peakRssMiB', 'peakRssMiB'],
    ['steadyPssMiB', 'steadyMedianPssMiB'],
    ['peakPssMiB', 'peakPssMiB'],
    ['childrenPeakRssMiB', 'childrenPeakRssMiB'],
    ['childrenPeakPssMiB', 'childrenPeakPssMiB'],
    ['totalSteadyRssMiB', 'totalSteadyMedianRssMiB'],
    ['totalPeakRssMiB', 'totalPeakRssMiB'],
  ];
  const violations = [];
  const results = [];
  for (const [limitKey, metricKey] of checks) {
    const limit = limits[limitKey];
    if (limit === undefined || limit === null) continue;
    const measured = memorySummary[metricKey];
    const deltaMiB = +(measured - limit).toFixed(2);
    const pctOver = limit > 0 ? +(((measured - limit) / limit) * 100).toFixed(1) : measured > 0 ? 100 : 0;
    const ok = measured <= limit;
    const row = { metric: limitKey, measuredMiB: measured, limitMiB: limit, deltaMiB, pctOver, ok };
    results.push(row);
    if (!ok) violations.push(row);
  }
  return { ok: violations.length === 0, results, violations };
}

/** Build the isolated server environment with every home-like variable pointed at the temp dir. */
export function buildIsolatedEnv({ home, ccsDir, reportsDir, registryDir, stubsDir, xdgRuntimeDir }) {
  return {
    PATH: `${stubsDir}:/usr/bin:/bin`,
    LANG: 'C.UTF-8',
    TZ: 'UTC',
    USER: 'perfuser',
    LOGNAME: 'perfuser',
    HOME: home,
    CCS_HOME: home,
    CCS_DIR: ccsDir,
    USERPROFILE: home,
    APPDATA: path.join(home, 'AppData', 'Roaming'),
    LOCALAPPDATA: path.join(home, 'AppData', 'Local'),
    XDG_CONFIG_HOME: path.join(home, '.config'),
    XDG_DATA_HOME: path.join(home, '.local', 'share'),
    XDG_STATE_HOME: path.join(home, '.local', 'state'),
    XDG_CACHE_HOME: path.join(home, '.cache'),
    XDG_RUNTIME_DIR: xdgRuntimeDir,
    CODEX_HOME: path.join(home, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
    AAC_AGENT_REPORTS_DIR: reportsDir,
    AAC_PM_MAIL_REGISTRY_DIR: registryDir,
    AAC_STATUS_DOORBELL: 'off',
    AAC_AGENT_TRANSPORT: 'off',
    AAC_AGENT_VERBS: 'off',
    AAC_STORE_SHARE_BACKUP_DIR: 'off',
    AAC_CURSOR_BILLING_ENABLED: '0',
    AAC_CURSOR_API_BASE: 'http://127.0.0.1:1',
    SSH_AUTH_SOCK: '',
  };
}

/** Create fail-fast helper stubs so Status never touches real machines, phones, or pm-mail. */
function writeStubs(stubsDir) {
  fs.mkdirSync(stubsDir, { recursive: true });
  for (const name of ['ssh', 'simlock', 'browser-check', 'pm-mail']) {
    const file = path.join(stubsDir, name);
    fs.writeFileSync(file, '#!/bin/sh\nexit 1\n', { mode: 0o755 });
  }
}

function getFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      const port = typeof addr === 'object' && addr ? addr.port : 0;
      srv.close(() => resolve(port));
    });
  });
}

function httpRequest({ port, method = 'GET', reqPath, headers = {}, body = null, agent, timeoutMs = 60_000 }) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        hostname: '127.0.0.1',
        port,
        path: reqPath,
        method,
        headers,
        agent,
        timeout: timeoutMs,
      },
      (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(chunk));
        res.on('end', () => {
          const buf = Buffer.concat(chunks);
          resolve({
            status: res.statusCode ?? 0,
            headers: res.headers,
            body: buf,
          });
        });
      }
    );
    req.on('timeout', () => {
      req.destroy(new Error(`HTTP timeout after ${timeoutMs} ms (${method} ${reqPath})`));
    });
    req.on('error', reject);
    if (body !== null) req.write(body);
    req.end();
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForHealth(port, child, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`Dashboard server exited before listening (code=${child.exitCode}).`);
    }
    try {
      const res = await httpRequest({ port, reqPath: '/api/health', timeoutMs: 2000 });
      if (res.status === 200) return;
    } catch {
      // Not listening yet.
    }
    await sleep(200);
  }
  throw new Error(`Dashboard server did not answer /api/health within ${timeoutMs} ms.`);
}

async function signInLoopback(port) {
  const baseHeaders = {
    Origin: `http://127.0.0.1:${port}`,
    Host: `127.0.0.1:${port}`,
    Accept: 'application/json',
  };
  const setupCheck = await httpRequest({ port, reqPath: '/api/auth/setup', headers: baseHeaders });
  if (setupCheck.status !== 200) {
    throw new Error(`GET /api/auth/setup answered ${setupCheck.status}`);
  }
  const parsed = JSON.parse(setupCheck.body.toString('utf8'));
  const target = parsed.configured ? '/api/auth/login' : '/api/auth/setup';
  const payload = JSON.stringify(
    parsed.configured
      ? { username: FIXTURE_USER, password: FIXTURE_PASS, rememberMe: true }
      : { username: FIXTURE_USER, password: FIXTURE_PASS }
  );
  const reply = await httpRequest({
    port,
    method: 'POST',
    reqPath: target,
    headers: {
      ...baseHeaders,
      'Content-Type': 'application/json',
      'Content-Length': Buffer.byteLength(payload),
    },
    body: payload,
  });
  const setCookies = reply.headers['set-cookie'] ?? [];
  const cookie = (Array.isArray(setCookies) ? setCookies : [setCookies])
    .map((value) => String(value).split(';', 1)[0])
    .filter(Boolean)
    .join('; ');
  if (reply.status < 200 || reply.status >= 300 || !cookie) {
    throw new Error(
      `POST ${target} failed (status=${reply.status}): ${reply.body.toString('utf8').slice(0, 200)}`
    );
  }
  return cookie;
}

async function runPollLoop(route, port, cookie, startedMs, durationMs, records) {
  const agent = new http.Agent({ keepAlive: true, maxSockets: 1 });
  let due = startedMs + route.offsetSec * 1000;
  try {
    while (true) {
      const now = Date.now();
      if (now - startedMs >= durationMs) break;
      if (now < due) {
        await sleep(Math.min(due - now, 250));
        continue;
      }
      due += route.periodSec * 1000;
      const callStart = Date.now();
      let status = 0;
      let bytes = 0;
      let encoding = '';
      let error = null;
      try {
        const res = await httpRequest({
          port,
          reqPath: route.path,
          agent,
          timeoutMs: 60_000,
          headers: {
            Accept: 'application/json',
            'Accept-Encoding': 'gzip, deflate, br',
            Origin: `http://127.0.0.1:${port}`,
            Host: `127.0.0.1:${port}`,
            Cookie: cookie,
          },
        });
        status = res.status;
        bytes = res.body.length;
        encoding = String(res.headers['content-encoding'] ?? '');
      } catch (err) {
        status = -1;
        error = err instanceof Error ? err.message : String(err);
      }
      const ms = Date.now() - callStart;
      records.push({
        tSec: +((callStart - startedMs) / 1000).toFixed(2),
        route: route.name,
        status,
        bytes,
        encoding,
        ms,
        ...(error ? { error } : {}),
      });
    }
  } finally {
    agent.destroy();
  }
}

function summarizeRoutes(records) {
  const byRoute = new Map();
  for (const row of records) {
    if (!byRoute.has(row.route)) byRoute.set(row.route, []);
    byRoute.get(row.route).push(row);
  }
  const summary = {};
  for (const route of POLL_PLAN) {
    const list = byRoute.get(route.name) ?? [];
    const ok = list.filter((r) => r.status === 200);
    const msList = list.map((r) => r.ms);
    const warmMs = msList.length > 1 ? msList.slice(1) : msList;
    summary[route.name] = {
      calls: list.length,
      ok: ok.length,
      failed: list.length - ok.length,
      medianBytes: Math.round(median(ok.map((r) => r.bytes))),
      firstMs: msList[0] ?? 0,
      p50Ms: +median(warmMs).toFixed(1),
      p95Ms: +percentile(warmMs, 0.95).toFixed(1),
      maxMs: msList.length ? Math.max(...msList) : 0,
    };
  }
  return summary;
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return;
  try {
    child.kill('SIGTERM');
  } catch {
    return;
  }
  const deadline = Date.now() + 10_000;
  while (child.exitCode === null && Date.now() < deadline) {
    await sleep(100);
  }
  if (child.exitCode === null) {
    try {
      child.kill('SIGKILL');
    } catch {
      // Already exited.
    }
  }
}

function checkBuiltArtifacts(repoRoot) {
  const launcher = path.join(
    repoRoot,
    'dist',
    'native',
    process.platform === 'win32' ? 'ai-account-center.exe' : 'ai-account-center'
  );
  const uiEntry = path.join(repoRoot, 'dist', 'ui', 'app', 'index.html');
  const storeMod = path.join(
    repoRoot,
    'dist',
    'native',
    `aac-store.${process.platform}-${process.arch}.node`
  );
  const missing = [];
  if (!fs.existsSync(launcher)) missing.push(path.relative(repoRoot, launcher));
  if (!fs.existsSync(uiEntry)) missing.push(path.relative(repoRoot, uiEntry));
  if (!fs.existsSync(storeMod)) missing.push(path.relative(repoRoot, storeMod));
  if (missing.length) {
    throw new Error(
      `Missing built artifacts (${missing.join(', ')}). Run "bun run build && bun run store:build && bun run status:build" on a Rust build host first.`
    );
  }
  return { launcher, storeMod };
}

function parseCli(argv) {
  const args = argv.slice(2);
  const opts = {
    durationSec: 300,
    steadyAfterSec: 120,
    sampleIntervalSec: 2,
    seed: DEFAULT_OPTIONS.seed,
    configPath: DEFAULT_CONFIG_PATH,
    outPath: null,
    fixtureDir: null,
    keep: false,
    check: true,
    json: false,
    help: false,
  };
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    const next = () => {
      i += 1;
      if (i >= args.length) throw new Error(`Missing value for ${arg}`);
      return args[i];
    };
    if (arg === '--duration') opts.durationSec = Number(next());
    else if (arg === '--steady-after') opts.steadyAfterSec = Number(next());
    else if (arg === '--sample-interval') opts.sampleIntervalSec = Number(next());
    else if (arg === '--seed') opts.seed = Number(next());
    else if (arg === '--config') opts.configPath = path.resolve(next());
    else if (arg === '--out') opts.outPath = path.resolve(next());
    else if (arg === '--fixture-dir') opts.fixtureDir = path.resolve(next());
    else if (arg === '--keep') opts.keep = true;
    else if (arg === '--no-check' || arg === '--measure-only') opts.check = false;
    else if (arg === '--json') opts.json = true;
    else if (arg === '-h' || arg === '--help') opts.help = true;
    else throw new Error(`Unknown argument: ${arg}`);
  }
  return opts;
}

export async function runMemoryBudget(opts) {
  if (process.platform !== 'linux') {
    throw new Error(`memory-budget requires Linux /proc sampling (current platform: ${process.platform}).`);
  }
  const wallStart = Date.now();
  const { launcher, storeMod } = checkBuiltArtifacts(REPO_ROOT);

  let thresholdConfig = null;
  if (opts.check) {
    if (!fs.existsSync(opts.configPath)) {
      throw new Error(`Threshold config not found: ${opts.configPath}`);
    }
    thresholdConfig = JSON.parse(fs.readFileSync(opts.configPath, 'utf8'));
  }

  const runRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-perf-memory-'));
  let child = null;
  let samplerTimer = null;

  const cleanup = async () => {
    if (samplerTimer) {
      clearInterval(samplerTimer);
      samplerTimer = null;
    }
    await stopChild(child);
    if (!opts.keep) {
      fs.rmSync(runRoot, { recursive: true, force: true });
    }
  };

  const onSignal = (sig) => {
    cleanup().finally(() => {
      process.exitCode = sig === 'SIGINT' ? 130 : 143;
      process.exit();
    });
  };
  process.once('SIGINT', onSignal);
  process.once('SIGTERM', onSignal);

  try {
    let fixture;
    const fixtureStart = Date.now();
    if (opts.fixtureDir) {
      const copied = path.join(runRoot, 'fixture');
      fs.cpSync(opts.fixtureDir, copied, { recursive: true });
      const manifest = JSON.parse(fs.readFileSync(path.join(copied, 'fixture-manifest.json'), 'utf8'));
      fixture = {
        ...manifest,
        root: copied,
        home: path.join(copied, 'home'),
        ccsDir: path.join(copied, 'home', '.ccs'),
        reportsDir: path.join(copied, 'agent-reports'),
        registryDir: path.join(copied, 'agent-reports', 'pm-mail-registry'),
      };
    } else {
      fixture = await generateMemoryFixture(path.join(runRoot, 'fixture'), {
        seed: opts.seed,
        storeModule: storeMod,
      });
    }
    const fixtureMs = Date.now() - fixtureStart;

    const stubsDir = path.join(runRoot, 'stubs');
    const xdgRuntimeDir = path.join(runRoot, 'xdg');
    const logsDir = path.join(runRoot, 'logs');
    writeStubs(stubsDir);
    fs.mkdirSync(xdgRuntimeDir, { recursive: true, mode: 0o700 });
    fs.mkdirSync(logsDir, { recursive: true });

    const port = await getFreePort();
    const env = buildIsolatedEnv({
      home: fixture.home,
      ccsDir: fixture.ccsDir,
      reportsDir: fixture.reportsDir,
      registryDir: fixture.registryDir,
      stubsDir,
      xdgRuntimeDir,
    });

    const stdoutFd = fs.openSync(path.join(logsDir, 'server-stdout.log'), 'w');
    const stderrFd = fs.openSync(path.join(logsDir, 'server-stderr.log'), 'w');
    const bootStart = Date.now();
    child = spawn(
      launcher,
      ['dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(port), '--config-dir', fixture.ccsDir],
      {
        cwd: REPO_ROOT,
        env,
        stdio: ['ignore', stdoutFd, stderrFd],
      }
    );
    fs.closeSync(stdoutFd);
    fs.closeSync(stderrFd);

    await waitForHealth(port, child, 60_000);
    const bootMs = Date.now() - bootStart;
    const cookie = await signInLoopback(port);

    const pollStart = Date.now();
    const durationMs = opts.durationSec * 1000;
    const samples = [];
    const takeSample = () => {
      if (!child?.pid || child.exitCode !== null) return;
      const sample = sampleProcessTree(child.pid, (Date.now() - pollStart) / 1000);
      if (sample) samples.push(sample);
    };
    takeSample();
    samplerTimer = setInterval(takeSample, Math.max(200, opts.sampleIntervalSec * 1000));

    const routeRecords = [];
    await Promise.all(
      POLL_PLAN.map((route) => runPollLoop(route, port, cookie, pollStart, durationMs, routeRecords))
    );
    takeSample();
    clearInterval(samplerTimer);
    samplerTimer = null;

    if (child.exitCode !== null) {
      throw new Error(`Dashboard server exited unexpectedly during run (code=${child.exitCode}).`);
    }

    const memory = summarizeSamples(samples, opts.steadyAfterSec);
    const routes = summarizeRoutes(routeRecords);
    const routeFailures = Object.entries(routes)
      .filter(([, row]) => row.calls > 0 && row.ok === 0)
      .map(([name, row]) => `${name}: 0/${row.calls} HTTP 200`);

    const threshold = thresholdConfig ? evaluateThresholds(memory, thresholdConfig) : null;

    const summary = {
      schemaVersion: 1,
      measuredAt: new Date().toISOString(),
      host: os.hostname(),
      seed: fixture.seed,
      durationSec: opts.durationSec,
      sampleIntervalSec: opts.sampleIntervalSec,
      timingMs: {
        fixture: fixtureMs,
        boot: bootMs,
        poll: Date.now() - pollStart,
        total: Date.now() - wallStart,
      },
      fixtureCounts: fixture.counts,
      store: fixture.store,
      memory,
      routes,
      routeFailures,
      threshold,
      ...(opts.keep ? { runRoot } : {}),
    };

    if (opts.outPath) {
      fs.mkdirSync(path.dirname(opts.outPath), { recursive: true });
      fs.writeFileSync(opts.outPath, `${JSON.stringify(summary, null, 2)}\n`);
    }

    return summary;
  } finally {
    process.removeListener('SIGINT', onSignal);
    process.removeListener('SIGTERM', onSignal);
    await cleanup();
  }
}

function formatHumanSummary(summary) {
  const m = summary.memory;
  const lines = [
    `[i] AAC memory budget run (${summary.durationSec}s, seed=${summary.seed}, samples=${m.sampleCount})`,
    `    timing: fixture=${(summary.timingMs.fixture / 1000).toFixed(1)}s boot=${(summary.timingMs.boot / 1000).toFixed(2)}s total=${(summary.timingMs.total / 1000).toFixed(1)}s`,
    `    server steady (t>=${m.steadyAfterSec}s): RSS=${m.steadyMedianRssMiB} MiB, PSS=${m.steadyMedianPssMiB} MiB`,
    `    server peak:               RSS=${m.peakRssMiB} MiB, PSS=${m.peakPssMiB} MiB`,
    `    children peak:             RSS=${m.childrenPeakRssMiB} MiB, PSS=${m.childrenPeakPssMiB} MiB (max children=${m.maxChildren})`,
    `    total peak:                RSS=${m.totalPeakRssMiB} MiB, PSS=${m.totalPeakPssMiB} MiB`,
  ];
  for (const [name, row] of Object.entries(summary.routes)) {
    lines.push(
      `    route ${name.padEnd(13)} ${String(row.ok).padStart(2)}/${String(row.calls).padEnd(2)} ok  bytes=${String(row.medianBytes).padStart(8)}  first=${String(row.firstMs).padStart(5)}ms  p50=${String(row.p50Ms).padStart(5)}ms  max=${String(row.maxMs).padStart(5)}ms`
    );
  }
  if (summary.routeFailures.length) {
    lines.push(`[X] Route failures: ${summary.routeFailures.join('; ')}`);
  }
  if (summary.threshold) {
    if (summary.threshold.ok) {
      lines.push('[OK] Memory within all configured limits:');
      for (const r of summary.threshold.results) {
        lines.push(`     ${r.metric}: ${r.measuredMiB} MiB <= ${r.limitMiB} MiB (${r.deltaMiB >= 0 ? '+' : ''}${r.deltaMiB} MiB)`);
      }
    } else {
      lines.push('[X] Memory budget EXCEEDED:');
      for (const v of summary.threshold.violations) {
        lines.push(
          `     EXCEEDED ${v.metric}: ${v.measuredMiB} MiB > limit ${v.limitMiB} MiB (+${v.deltaMiB} MiB, +${v.pctOver}%)`
        );
      }
    }
  }
  return `${lines.join('\n')}\n`;
}

async function main(argv) {
  const opts = parseCli(argv);
  if (opts.help) {
    process.stdout.write(
      'Usage: node scripts/perf/memory-budget.mjs [--duration 300] [--steady-after 120] [--sample-interval 2] [--seed 20261125] [--config config/memory-budget.json] [--out <file.json>] [--fixture-dir <dir>] [--keep] [--no-check] [--json]\n'
    );
    return 0;
  }
  const summary = await runMemoryBudget(opts);
  if (opts.json) {
    process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
  } else {
    process.stdout.write(formatHumanSummary(summary));
  }
  if (summary.routeFailures.length > 0) return 1;
  if (summary.threshold && !summary.threshold.ok) return 1;
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
