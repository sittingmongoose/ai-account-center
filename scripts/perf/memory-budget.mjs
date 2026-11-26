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
import { promisify } from 'node:util';
import { brotliDecompress, gunzip, inflate } from 'node:zlib';
import { DEFAULT_OPTIONS, generateMemoryFixture } from './memory-fixture.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..', '..');
const DEFAULT_CONFIG_PATH = path.join(REPO_ROOT, 'config', 'memory-budget.json');
const KIB_PER_MIB = 1024;
const MIN_STEADY_SAMPLES_FLOOR = 3;
const POLL_DISPATCH_GRACE_MS = 250;
const ANALYTICS_DECODERS = new Map([
  ['br', promisify(brotliDecompress)],
  ['gzip', promisify(gunzip)],
  ['deflate', promisify(inflate)],
]);
const MIN_SAMPLE_INTERVAL_SEC = 0.2;

/** Required budget ceilings in config/memory-budget.json for check mode. */
export const REQUIRED_LIMIT_KEYS = Object.freeze([
  'steadyRssMiB',
  'peakRssMiB',
  'childrenPeakRssMiB',
]);

const THRESHOLD_CHECKS = Object.freeze([
  ['steadyRssMiB', 'steadyMedianRssKiB', 'steadyMedianRssMiB'],
  ['peakRssMiB', 'peakRssKiB', 'peakRssMiB'],
  ['steadyPssMiB', 'steadyMedianPssKiB', 'steadyMedianPssMiB'],
  ['peakPssMiB', 'peakPssKiB', 'peakPssMiB'],
  ['childrenPeakRssMiB', 'childrenPeakRssKiB', 'childrenPeakRssMiB'],
  ['childrenPeakPssMiB', 'childrenPeakPssKiB', 'childrenPeakPssMiB'],
  ['totalSteadyRssMiB', 'totalSteadyMedianRssKiB', 'totalSteadyMedianRssMiB'],
  ['totalPeakRssMiB', 'totalPeakRssKiB', 'totalPeakRssMiB'],
]);

/** Every limit key accepted in config/memory-budget.json. */
export const ALLOWED_LIMIT_KEYS = Object.freeze(
  new Set(THRESHOLD_CHECKS.map(([limitKey]) => limitKey))
);

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

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Refuse symlinks and unexpected file types in fixture inputs before traversing them. */
export function assertNoSymlinks(root) {
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    const stat = fs.lstatSync(current);
    if (stat.isSymbolicLink()) {
      throw new Error(`Fixture input contains a symlink: ${current}`);
    }
    if (stat.isDirectory()) {
      for (const entry of fs.readdirSync(current)) {
        stack.push(path.join(current, entry));
      }
      continue;
    }
    if (stat.isFile()) continue;
    throw new Error(`Fixture input contains an unexpected file type: ${current}`);
  }
}

/** Verify `target` resolves inside `root` (no .. escape or symlink escape). */
export function isPathInside(root, target) {
  const rel = path.relative(root, target);
  return rel !== '..' && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel);
}

export function assertResolvedInside(root, file) {
  const realRoot = fs.realpathSync(root);
  let realFile;
  try {
    realFile = fs.realpathSync(file);
  } catch {
    throw new Error(`Fixture root does not exist: ${file}`);
  }
  if (!isPathInside(realRoot, realFile)) {
    throw new Error(`Fixture root escapes the fixture: ${file} -> ${realFile}`);
  }
}

/** Return exit details when a child has exited or been terminated by a signal, else null. */
export function getChildExitStatus(child) {
  if (!child) return null;
  if (child.exitCode !== null || child.signalCode !== null) {
    return {
      exitCode: child.exitCode ?? null,
      signalCode: child.signalCode ?? null,
      reason:
        child.signalCode !== null && child.signalCode !== undefined
          ? `signal=${child.signalCode}`
          : `code=${child.exitCode}`,
    };
  }
  return null;
}

/** Parse /proc/<pid>/smaps_rollup text into KiB counters. */
export function parseSmapsRollup(text) {
  const out = {
    rssKiB: 0,
    pssKiB: 0,
    pssAnonKiB: 0,
    pssFileKiB: 0,
    swapKiB: 0,
    pssAvailable: true,
  };
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
  // PSS apportions shared mappings; VmRSS is not PSS. Mark unavailable rather than
  // reporting RSS as PSS (would fail a valid PSS budget).
  return { rssKiB, pssKiB: null, pssAnonKiB: null, pssFileKiB: null, swapKiB, pssAvailable: false };
}

function readProcStatState(text) {
  const close = text.lastIndexOf(')');
  if (close < 0) return null;
  const fields = text.slice(close + 2).trim().split(/\s+/);
  return fields[0] ?? null;
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

function isPidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  try {
    const state = readProcStatState(fs.readFileSync(`/proc/${pid}/stat`, 'utf8'));
    if (state === 'Z' || state === 'X') return false;
  } catch {
    return false;
  }
  return true;
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
  let rootStatText;
  let rootStat;
  try {
    rootStatText = fs.readFileSync(`/proc/${rootPid}/stat`, 'utf8');
    const state = readProcStatState(rootStatText);
    if (state === 'Z' || state === 'X') return null;
    rootStat = parseProcStat(rootStatText);
  } catch {
    return null;
  }
  const server = readProcMemory(rootPid);
  if (!server) return null;
  const parentByPid = listProcParents();
  const kids = collectDescendants(rootPid, parentByPid);
  let childrenRssKiB = 0;
  let childrenPssKiB = 0;
  let childrenPssAvailable = true;
  for (const kid of kids) {
    const mem = readProcMemory(kid);
    if (!mem) continue;
    childrenRssKiB += mem.rssKiB;
    if (mem.pssAvailable === false || mem.pssKiB == null) {
      childrenPssAvailable = false;
    } else {
      childrenPssKiB += mem.pssKiB;
    }
  }
  const serverPssAvailable = server.pssAvailable !== false && server.pssKiB != null;
  const totalPssAvailable = serverPssAvailable && childrenPssAvailable;
  return {
    tSec: +elapsedSec.toFixed(2),
    serverRssKiB: server.rssKiB,
    serverPssKiB: serverPssAvailable ? server.pssKiB : null,
    serverAnonKiB: serverPssAvailable ? server.pssAnonKiB : null,
    serverFileKiB: serverPssAvailable ? server.pssFileKiB : null,
    serverSwapKiB: server.swapKiB,
    pssAvailable: totalPssAvailable,
    serverPssAvailable,
    threads: rootStat?.threads ?? 1,
    childrenCount: kids.length,
    childrenRssKiB,
    childrenPssKiB: childrenPssAvailable ? childrenPssKiB : null,
    totalRssKiB: server.rssKiB + childrenRssKiB,
    totalPssKiB: totalPssAvailable ? server.pssKiB + childrenPssKiB : null,
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

/** Minimum valid steady-window samples required for check mode. */
export function minRequiredSteadySamples(durationSec, steadyAfterSec, sampleIntervalSec) {
  const steadyWindowSec = durationSec - steadyAfterSec;
  if (
    !Number.isFinite(steadyWindowSec) ||
    steadyWindowSec <= 0 ||
    !Number.isFinite(sampleIntervalSec) ||
    sampleIntervalSec <= 0
  ) {
    return 0;
  }
  const expected = Math.floor(steadyWindowSec / sampleIntervalSec) + 1;
  return Math.max(MIN_STEADY_SAMPLES_FLOOR, Math.ceil(expected * 0.5));
}

/** Validate timing options before generating the fixture or starting the server. */
export function validateRunOptions(opts, pollPlan = POLL_PLAN) {
  if (!isPlainObject(opts)) {
    throw new Error('Runner options must be an object.');
  }
  const { durationSec, steadyAfterSec, sampleIntervalSec, check = true } = opts;
  if (typeof durationSec !== 'number' || !Number.isFinite(durationSec) || durationSec <= 0) {
    throw new Error(`Invalid durationSec (${durationSec}): must be a finite number > 0.`);
  }
  if (
    typeof steadyAfterSec !== 'number' ||
    !Number.isFinite(steadyAfterSec) ||
    steadyAfterSec < 0
  ) {
    throw new Error(`Invalid steadyAfterSec (${steadyAfterSec}): must be a finite number >= 0.`);
  }
  if (
    typeof sampleIntervalSec !== 'number' ||
    !Number.isFinite(sampleIntervalSec) ||
    sampleIntervalSec <= 0
  ) {
    throw new Error(
      `Invalid sampleIntervalSec (${sampleIntervalSec}): must be a finite number > 0.`
    );
  }
  if (sampleIntervalSec < MIN_SAMPLE_INTERVAL_SEC) {
    throw new Error(
      `Invalid sampleIntervalSec (${sampleIntervalSec}): must be >= ${MIN_SAMPLE_INTERVAL_SEC} (minimum supported sample interval).`
    );
  }
  if (opts.seed !== undefined && (typeof opts.seed !== 'number' || !Number.isFinite(opts.seed))) {
    throw new Error(`Invalid seed (${opts.seed}): must be a finite number.`);
  }
  if (!Array.isArray(pollPlan) || pollPlan.length === 0) {
    throw new Error('Poll plan must contain at least one route.');
  }

  if (check) {
    if (durationSec <= steadyAfterSec) {
      throw new Error(
        `Check mode requires durationSec (${durationSec}s) > steadyAfterSec (${steadyAfterSec}s) to collect steady-window samples.`
      );
    }
    const maxOffsetSec = Math.max(...pollPlan.map((route) => route.offsetSec));
    if (durationSec <= maxOffsetSec) {
      throw new Error(
        `Check mode requires durationSec (${durationSec}s) > max route offset (${maxOffsetSec}s) so every route is polled.`
      );
    }
    const steadyWindowSec = durationSec - steadyAfterSec;
    const expectedSteady = Math.floor(steadyWindowSec / sampleIntervalSec) + 1;
    if (expectedSteady < MIN_STEADY_SAMPLES_FLOOR) {
      throw new Error(
        `Check mode steady window (${steadyWindowSec}s at ${sampleIntervalSec}s interval) allows only ${expectedSteady} sample(s); need at least ${MIN_STEADY_SAMPLES_FLOOR}.`
      );
    }
  }
}

/** Validate a threshold config object; refuses missing required limits, non-finite values, and unknown keys. */
export function validateThresholdConfig(
  config,
  { requireSchemaVersion = false, source = 'threshold config' } = {}
) {
  if (!isPlainObject(config)) {
    throw new Error(`Invalid ${source}: expected a JSON object.`);
  }
  if (requireSchemaVersion || config.schemaVersion !== undefined) {
    if (config.schemaVersion !== 1) {
      throw new Error(
        `Invalid ${source}: unsupported schemaVersion ${JSON.stringify(config.schemaVersion)} (expected 1).`
      );
    }
  }
  if (!isPlainObject(config.limits)) {
    throw new Error(`Invalid ${source}: missing "limits" object.`);
  }

  const limitKeys = Object.keys(config.limits);
  const unknownKeys = limitKeys.filter((key) => !ALLOWED_LIMIT_KEYS.has(key));
  if (unknownKeys.length > 0) {
    throw new Error(
      `Invalid ${source}: unknown limit key(s) ${unknownKeys.map((k) => `"${k}"`).join(', ')} (allowed: ${[...ALLOWED_LIMIT_KEYS].join(', ')}).`
    );
  }

  for (const key of REQUIRED_LIMIT_KEYS) {
    if (!Object.hasOwn(config.limits, key)) {
      throw new Error(`Invalid ${source}: missing required limit "${key}".`);
    }
  }

  for (const key of limitKeys) {
    const val = config.limits[key];
    if (typeof val !== 'number' || !Number.isFinite(val) || val < 0) {
      throw new Error(
        `Invalid ${source}: limit "${key}" must be a finite non-negative number (got ${JSON.stringify(val)}).`
      );
    }
  }

  return config;
}

/** Read and validate config/memory-budget.json before generating the fixture. */
export function loadThresholdConfig(configPath) {
  if (!fs.existsSync(configPath)) {
    throw new Error(`Threshold config not found: ${configPath}`);
  }
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  } catch (err) {
    throw new Error(
      `Invalid threshold config JSON (${configPath}): ${err instanceof Error ? err.message : String(err)}`
    );
  }
  return validateThresholdConfig(parsed, { requireSchemaVersion: true, source: configPath });
}

/**
 * Compute steady median (after steadyAfterSec), peak, and children peak summary.
 * Retains exact KiB values for unrounded threshold comparison.
 * Refuses empty steady windows unless allowSteadyFallback is explicitly enabled (measure-only runs).
 */
export function summarizeSamples(samples, steadyAfterSec = 120, options = {}) {
  if (!samples.length) {
    throw new Error('No memory samples were collected from /proc.');
  }
  const allowSteadyFallback = options?.allowSteadyFallback === true;
  const steadyWindow = samples.filter((s) => s.tSec >= steadyAfterSec);
  if (steadyWindow.length === 0 && !allowSteadyFallback) {
    throw new Error(
      `No memory samples were collected in the steady window (t >= ${steadyAfterSec}s; collected ${samples.length} earlier sample(s)).`
    );
  }
  const steadySource = steadyWindow.length > 0 ? steadyWindow : samples;

  const completePss = (source, key, reduce) => {
    const values = source.map((sample) => sample[key]);
    return values.every(Number.isFinite) ? reduce(values) : null;
  };
  const peak = (values) => Math.max(...values);
  const peakServerRssKiB = peak(samples.map((s) => s.serverRssKiB));
  const peakServerPssKiB = completePss(samples, 'serverPssKiB', peak);
  const childrenPeakRssKiB = peak(samples.map((s) => s.childrenRssKiB));
  const childrenPeakPssKiB = completePss(samples, 'childrenPssKiB', peak);
  const totalPeakRssKiB = peak(samples.map((s) => s.totalRssKiB));
  const totalPeakPssKiB = completePss(samples, 'totalPssKiB', peak);

  const steadyMedianRssKiB = median(steadySource.map((s) => s.serverRssKiB));
  const steadyMedianPssKiB = completePss(steadySource, 'serverPssKiB', median);
  const steadyMedianTotalRssKiB = median(steadySource.map((s) => s.totalRssKiB));
  const steadyMedianTotalPssKiB = completePss(steadySource, 'totalPssKiB', median);

  const lastSample = samples[samples.length - 1];

  const toMiBOrNull = (kib) => (kib == null ? null : toMiB(kib));
  return {
    sampleCount: samples.length,
    steadyAfterSec,
    steadySampleCount: steadyWindow.length,
    steadyUsedFallback: steadyWindow.length === 0,
    lastSampleSec: lastSample.tSec,
    pssAvailable: totalPeakPssKiB !== null,
    steadyMedianRssKiB,
    steadyMedianPssKiB,
    peakRssKiB: peakServerRssKiB,
    peakPssKiB: peakServerPssKiB,
    childrenPeakRssKiB,
    childrenPeakPssKiB,
    totalSteadyMedianRssKiB: steadyMedianTotalRssKiB,
    totalSteadyMedianPssKiB: steadyMedianTotalPssKiB,
    totalPeakRssKiB,
    totalPeakPssKiB,
    endRssKiB: lastSample.serverRssKiB,
    endPssKiB: lastSample.serverPssKiB ?? null,
    steadyMedianRssMiB: toMiB(steadyMedianRssKiB),
    steadyMedianPssMiB: toMiBOrNull(steadyMedianPssKiB),
    peakRssMiB: toMiB(peakServerRssKiB),
    peakPssMiB: toMiBOrNull(peakServerPssKiB),
    childrenPeakRssMiB: toMiB(childrenPeakRssKiB),
    childrenPeakPssMiB: toMiBOrNull(childrenPeakPssKiB),
    totalSteadyMedianRssMiB: toMiB(steadyMedianTotalRssKiB),
    totalSteadyMedianPssMiB: toMiBOrNull(steadyMedianTotalPssKiB),
    totalPeakRssMiB: toMiB(totalPeakRssKiB),
    totalPeakPssMiB: toMiBOrNull(totalPeakPssKiB),
    endRssMiB: toMiB(lastSample.serverRssKiB),
    endPssMiB: toMiBOrNull(lastSample.serverPssKiB ?? null),
    maxThreads: Math.max(...samples.map((s) => s.threads)),
    maxChildren: Math.max(...samples.map((s) => s.childrenCount)),
    samplingNote:
      'Child memory peaks are based on /proc samples. A helper that starts and exits between samples may be missed.',
  };
}

/** Check whether steady-window sampling met check-mode sufficiency requirements. */
export function evaluateSampleFailures(memory, { durationSec, steadyAfterSec, sampleIntervalSec }) {
  const failures = [];
  if (!memory || memory.sampleCount <= 0) {
    failures.push('No memory samples were collected from /proc.');
    return failures;
  }
  if (memory.steadyUsedFallback) {
    failures.push(
      `Steady window (t >= ${steadyAfterSec}s) collected 0 samples; startup fallback is not permitted in check mode.`
    );
  }
  const minSteady = minRequiredSteadySamples(durationSec, steadyAfterSec, sampleIntervalSec);
  if (memory.steadySampleCount < minSteady) {
    failures.push(
      `Insufficient steady-window samples (t >= ${steadyAfterSec}s): collected ${memory.steadySampleCount}, required >= ${minSteady}.`
    );
  }
  const minLastSec = Math.max(steadyAfterSec, durationSec - Math.max(sampleIntervalSec * 3, 5));
  if (
    typeof memory.lastSampleSec !== 'number' ||
    !Number.isFinite(memory.lastSampleSec) ||
    memory.lastSampleSec < minLastSec
  ) {
    failures.push(
      `Memory sampling ended too early: last sample at t=${memory.lastSampleSec ?? 'none'}s, required >= ${minLastSec.toFixed(1)}s.`
    );
  }
  return failures;
}

/** Check measured metrics against config/memory-budget.json limits using unrounded values. */
export function evaluateThresholds(memorySummary, config) {
  validateThresholdConfig(config);
  if (!isPlainObject(memorySummary)) {
    throw new Error('Memory summary must be an object.');
  }
  if (memorySummary.steadyUsedFallback === true || memorySummary.steadySampleCount === 0) {
    throw new Error(
      'Cannot evaluate thresholds without steady-window samples (steadyUsedFallback=true or steadySampleCount=0).'
    );
  }

  const limits = config.limits;
  const violations = [];
  const results = [];
  for (const [limitKey, kibKey, mibKey] of THRESHOLD_CHECKS) {
    if (!Object.hasOwn(limits, limitKey)) continue;
    const limit = limits[limitKey];
    const limitKiB = limit * KIB_PER_MIB;
    let measuredKiB;
    let exactMeasuredMiB;
    if (typeof memorySummary[kibKey] === 'number' && Number.isFinite(memorySummary[kibKey])) {
      measuredKiB = memorySummary[kibKey];
      exactMeasuredMiB = measuredKiB / KIB_PER_MIB;
    } else if (
      typeof memorySummary[mibKey] === 'number' &&
      Number.isFinite(memorySummary[mibKey])
    ) {
      exactMeasuredMiB = memorySummary[mibKey];
      measuredKiB = exactMeasuredMiB * KIB_PER_MIB;
    } else {
      if (limitKey.includes('Pss')) {
        throw new Error(`Memory summary lacks valid PSS coverage for limit "${limitKey}".`);
      }
      throw new Error(
        `Memory summary is missing a finite value for "${limitKey}" (${kibKey}/${mibKey}).`
      );
    }

    const deltaKiB = measuredKiB - limitKiB;
    const exactDeltaMiB = deltaKiB / KIB_PER_MIB;
    const ok = deltaKiB <= 0;

    let measuredMiB = +exactMeasuredMiB.toFixed(2);
    let deltaMiB = +exactDeltaMiB.toFixed(2);
    if (!ok && (measuredMiB <= limit || deltaMiB <= 0)) {
      measuredMiB = +exactMeasuredMiB.toFixed(4);
      deltaMiB = +exactDeltaMiB.toFixed(4);
      if (measuredMiB <= limit || deltaMiB <= 0) {
        measuredMiB = exactMeasuredMiB;
        deltaMiB = exactDeltaMiB;
      }
    }
    const rawPct = limitKiB > 0 ? (deltaKiB / limitKiB) * 100 : deltaKiB > 0 ? 100 : 0;
    const pctOver =
      !ok && Math.abs(+rawPct.toFixed(1)) === 0 ? +rawPct.toFixed(4) || rawPct : +rawPct.toFixed(1);

    const row = {
      metric: limitKey,
      measuredMiB,
      limitMiB: limit,
      deltaMiB,
      measuredKiB,
      limitKiB,
      deltaKiB,
      pctOver,
      ok,
    };
    results.push(row);
    if (!ok) violations.push(row);
  }

  if (results.length < REQUIRED_LIMIT_KEYS.length) {
    throw new Error(
      `Threshold evaluation executed only ${results.length} check(s); expected at least ${REQUIRED_LIMIT_KEYS.length}.`
    );
  }
  return { ok: violations.length === 0, results, violations };
}

/** Build the isolated server environment with every home-like variable pointed at the temp dir. */
export function buildIsolatedEnv({
  home,
  ccsDir,
  reportsDir,
  registryDir,
  stubsDir,
  xdgRuntimeDir,
}) {
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

function httpRequest({
  port,
  method = 'GET',
  reqPath,
  headers = {},
  body = null,
  agent,
  timeoutMs = 60_000,
  signal = null,
}) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error(`HTTP request aborted (${method} ${reqPath})`));
      return;
    }
    const req = http.request(
      {
        hostname: '127.0.0.1',
        port,
        path: reqPath,
        method,
        headers,
        agent,
      },
      (res) => {
        const chunks = [];
        const onIncomplete = () =>
          fail(new Error(`HTTP response closed before completion (${method} ${reqPath})`));
        res.on('data', (chunk) => chunks.push(chunk));
        res.on('aborted', onIncomplete);
        res.on('error', fail);
        res.on('close', () => {
          if (!res.complete) onIncomplete();
        });
        res.on('end', () => {
          if (!res.complete) {
            onIncomplete();
            return;
          }
          cleanup();
          resolve({
            status: res.statusCode ?? 0,
            headers: res.headers,
            body: Buffer.concat(chunks),
          });
        });
      }
    );
    const fail = (error) => {
      cleanup();
      reject(error);
    };
    const cancel = (error) => {
      fail(error);
      req.destroy(error);
    };
    const onAbort = () => cancel(new Error(`HTTP request aborted (${method} ${reqPath})`));
    const deadline = setTimeout(() => {
      cancel(new Error(`HTTP deadline exceeded after ${timeoutMs} ms (${method} ${reqPath})`));
    }, timeoutMs);
    const cleanup = () => {
      clearTimeout(deadline);
      signal?.removeEventListener('abort', onAbort);
    };
    signal?.addEventListener('abort', onAbort, { once: true });
    req.on('error', fail);
    if (body !== null) req.write(body);
    req.end();
  });
}

function sleep(ms, signal = null) {
  if (ms <= 0 || signal?.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      if (signal) signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    if (signal) signal.addEventListener('abort', onAbort, { once: true });
  });
}

async function waitForHealth(port, signal, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (!signal.aborted && Date.now() < deadline) {
    try {
      const res = await httpRequest({
        port,
        reqPath: '/api/health',
        timeoutMs: Math.min(2000, deadline - Date.now()),
        signal,
      });
      if (res.status === 200) return;
    } catch {
      // Not listening yet.
    }
    await sleep(Math.min(200, deadline - Date.now()), signal);
  }
  if (signal.aborted) throw new Error('Dashboard health check aborted.');
  throw new Error(`Dashboard server did not answer /api/health within ${timeoutMs} ms.`);
}

async function signInLoopback(port, signal) {
  const baseHeaders = {
    Origin: `http://127.0.0.1:${port}`,
    Host: `127.0.0.1:${port}`,
    Accept: 'application/json',
  };
  const setupCheck = await httpRequest({
    port,
    reqPath: '/api/auth/setup',
    headers: baseHeaders,
    signal,
  });
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
    signal,
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

async function verifyAnalyticsReply(url, body, encoding) {
  const decode = ANALYTICS_DECODERS.get(encoding);
  if (encoding && encoding !== 'identity' && !decode) {
    throw new Error(`Unsupported Analytics content encoding: ${encoding}`);
  }
  const payload = JSON.parse((decode ? await decode(body) : body).toString('utf8'));
  if (payload?.stale === true || payload?.history?.status !== 'ok') {
    throw new Error('Analytics quota history is unavailable.');
  }
  if (!['ok', 'cached'].includes(payload?.activity?.status)) {
    throw new Error('Analytics activity is unavailable.');
  }
  if (
    payload.range?.preset !== url.searchParams.get('range') ||
    !Number.isFinite(payload.history.sampleCount) ||
    payload.history.sampleCount <= 0 ||
    !Array.isArray(payload.activity.byDay) ||
    payload.activity.byDay.length === 0
  ) {
    throw new Error('Analytics reply is missing quota or activity data for the requested range.');
  }
}

async function runPollLoop(route, port, cookie, startedMs, durationSec, records, abortSignal) {
  const agent = new http.Agent({ keepAlive: true, maxSockets: 1 });
  const url = new URL(route.path, 'http://127.0.0.1');
  const scheduled = expectedRouteCalls(route, durationSec);
  const dispatchDeadline = startedMs + durationSec * 1000 + POLL_DISPATCH_GRACE_MS;
  try {
    for (let slot = 0; slot < scheduled; slot += 1) {
      const due = startedMs + (route.offsetSec + slot * route.periodSec) * 1000;
      await sleep(Math.ceil(due - Date.now()), abortSignal);
      const callStart = Date.now();
      if (abortSignal.aborted || callStart >= dispatchDeadline) break;
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
          signal: abortSignal,
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
        if (status === 200 && url.pathname === '/api/accounts/analytics') {
          await verifyAnalyticsReply(url, res.body, encoding.trim().toLowerCase());
        }
      } catch (err) {
        if (abortSignal.aborted) break;
        if (status === 0) status = -1;
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

export function summarizeRoutes(records, pollPlan = POLL_PLAN) {
  const byRoute = new Map();
  for (const row of records) {
    if (!byRoute.has(row.route)) byRoute.set(row.route, []);
    byRoute.get(row.route).push(row);
  }
  const summary = {};
  for (const route of pollPlan) {
    const list = byRoute.get(route.name) ?? [];
    const ok = list.filter((r) => r.status === 200 && !r.error);
    const firstError = list.find((r) => r.error)?.error;
    const msList = list.map((r) => r.ms);
    const warmMs = msList.length > 1 ? msList.slice(1) : msList;
    summary[route.name] = {
      calls: list.length,
      ok: ok.length,
      failed: list.length - ok.length,
      ...(firstError ? { firstError } : {}),
      medianBytes: Math.round(median(ok.map((r) => r.bytes))),
      firstMs: msList[0] ?? 0,
      p50Ms: +median(warmMs).toFixed(1),
      p95Ms: +percentile(warmMs, 0.95).toFixed(1),
      maxMs: msList.length ? Math.max(...msList) : 0,
    };
  }
  return summary;
}

/** Expected number of scheduled calls for a route during [0, durationSec). */
export function expectedRouteCalls(route, durationSec) {
  if (
    !Number.isFinite(durationSec) ||
    durationSec <= 0 ||
    durationSec <= route.offsetSec ||
    !Number.isFinite(route.periodSec) ||
    route.periodSec <= 0
  ) {
    return 0;
  }
  return Math.floor((durationSec - route.offsetSec - 1e-9) / route.periodSec) + 1;
}

/**
 * Verify every route polled successfully across the full window:
 * - Zero poll failures allowed (later failures fail even after an initial 200)
 * - In check mode, every route must complete its scheduled polls (zero calls fails)
 */
export function evaluateRouteFailures(
  routes,
  durationSec = 300,
  pollPlan = POLL_PLAN,
  { check = true } = {}
) {
  const failures = [];
  for (const route of pollPlan) {
    const row = routes?.[route.name];
    const scheduled = expectedRouteCalls(route, durationSec);
    const minRequired = check ? Math.max(1, scheduled) : scheduled;

    if (!row || row.calls === 0) {
      if (minRequired > 0) {
        failures.push(`${route.name}: 0 polls completed (expected >= ${minRequired})`);
      }
      continue;
    }

    const failedCount =
      typeof row.failed === 'number' && Number.isFinite(row.failed)
        ? row.failed
        : Math.max(0, row.calls - row.ok);
    if (failedCount > 0 || row.ok < row.calls) {
      failures.push(
        `${route.name}: ${failedCount}/${row.calls} polls failed (${row.ok}/${row.calls} successful)` +
          (row.firstError ? `: ${row.firstError}` : '')
      );
      continue;
    }

    if (minRequired > 0 && row.calls < minRequired) {
      failures.push(
        `${route.name}: only ${row.calls}/${scheduled} polls completed across ${durationSec}s window (expected >= ${minRequired})`
      );
    }
  }
  return failures;
}

async function stopChild(child) {
  if (!child || getChildExitStatus(child) !== null) return;
  try {
    child.kill('SIGTERM');
  } catch {
    return;
  }
  const deadline = Date.now() + 10_000;
  while (getChildExitStatus(child) === null && Date.now() < deadline) {
    await sleep(100);
  }
  if (getChildExitStatus(child) === null) {
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

export function parseCli(argv) {
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
    throw new Error(
      `memory-budget requires Linux /proc sampling (current platform: ${process.platform}).`
    );
  }
  const pollPlan = opts.pollPlan ?? POLL_PLAN;
  validateRunOptions(opts, pollPlan);

  let thresholdConfig = null;
  if (opts.check) {
    thresholdConfig = loadThresholdConfig(opts.configPath);
  }

  const wallStart = Date.now();
  const artifacts = opts.launcherPath
    ? { launcher: opts.launcherPath, storeMod: null }
    : checkBuiltArtifacts(REPO_ROOT);

  const runRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-perf-memory-'));
  let child = null;
  let samplerTimer = null;
  let stoppingChild = false;

  const cleanup = async () => {
    if (samplerTimer) {
      clearInterval(samplerTimer);
      samplerTimer = null;
    }
    stoppingChild = true;
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
      assertNoSymlinks(opts.fixtureDir);
      fs.cpSync(opts.fixtureDir, copied, { recursive: true, dereference: false });
      assertNoSymlinks(copied);
      const manifest = JSON.parse(
        fs.readFileSync(path.join(copied, 'fixture-manifest.json'), 'utf8')
      );
      const ccsDir = path.join(copied, 'home', '.ccs');
      const storePath = path.join(ccsDir, 'store', 'aac.redb');
      if (
        opts.check &&
        (!manifest.store || !fs.statSync(storePath, { throwIfNoEntry: false })?.isFile())
      ) {
        throw new Error('Check mode requires a migrated store in --fixture-dir.');
      }
      fixture = {
        ...manifest,
        root: copied,
        home: path.join(copied, 'home'),
        ccsDir,
        reportsDir: path.join(copied, 'agent-reports'),
        registryDir: path.join(copied, 'agent-reports', 'pm-mail-registry'),
      };
    } else if (opts.launcherPath) {
      const fakeRoot = path.join(runRoot, 'fixture');
      const home = path.join(fakeRoot, 'home');
      const ccsDir = path.join(home, '.ccs');
      const reportsDir = path.join(fakeRoot, 'agent-reports');
      const registryDir = path.join(reportsDir, 'pm-mail-registry');
      fs.mkdirSync(ccsDir, { recursive: true });
      fs.mkdirSync(registryDir, { recursive: true });
      fixture = {
        seed: opts.seed,
        counts: {},
        store: null,
        root: fakeRoot,
        home,
        ccsDir,
        reportsDir,
        registryDir,
      };
    } else {
      fixture = await generateMemoryFixture(path.join(runRoot, 'fixture'), {
        seed: opts.seed,
        storeModule: artifacts.storeMod,
      });
      // The generator requires a new directory; verify it stays inside runRoot.
      assertResolvedInside(runRoot, fixture.root);
    }
    const fixtureMs = Date.now() - fixtureStart;

    const stubsDir = path.join(runRoot, 'stubs');
    const xdgRuntimeDir = path.join(runRoot, 'xdg');
    const logsDir = path.join(runRoot, 'logs');
    const fixtureRoot = path.join(runRoot, 'fixture');
    for (const candidate of [
      fixture.home,
      fixture.ccsDir,
      fixture.reportsDir,
      fixture.registryDir,
    ]) {
      assertResolvedInside(fixtureRoot, candidate);
      if (!fs.lstatSync(candidate).isDirectory()) {
        throw new Error(`Fixture root must be a directory: ${candidate}`);
      }
    }
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
    const launchArgs = opts.launcherArgs
      ? opts.launcherArgs({ port, ccsDir: fixture.ccsDir })
      : [
          'dashboard',
          '--no-open',
          '--host',
          '127.0.0.1',
          '--port',
          String(port),
          '--config-dir',
          fixture.ccsDir,
        ];
    child = spawn(artifacts.launcher, launchArgs, {
      cwd: REPO_ROOT,
      env,
      stdio: ['ignore', stdoutFd, stderrFd],
    });
    fs.closeSync(stdoutFd);
    fs.closeSync(stderrFd);

    let unexpectedExit = null;
    let spawnError = null;
    const childAbort = new AbortController();
    child.once('exit', (code, signal) => {
      if (stoppingChild || unexpectedExit) return;
      unexpectedExit = {
        exitCode: code ?? null,
        signalCode: signal ?? null,
        reason: signal ? `signal=${signal}` : `code=${code}`,
      };
      childAbort.abort();
    });
    child.once('error', (err) => {
      if (stoppingChild) return;
      spawnError = err;
      childAbort.abort();
    });

    const assertChildRunning = (phase) => {
      if (spawnError) {
        throw new Error(`Dashboard server failed ${phase}: ${spawnError.message}`);
      }
      const exitInfo = unexpectedExit ?? getChildExitStatus(child);
      if (exitInfo) {
        throw new Error(`Dashboard server exited unexpectedly ${phase} (${exitInfo.reason}).`);
      }
    };

    const samples = [];
    let pollStart = null;
    const takeSample = () => {
      if (!child?.pid || unexpectedExit || getChildExitStatus(child)) return;
      // Startup samples count toward peaks but precede even a steady window starting at t=0.
      const elapsedSec = pollStart === null ? -1 : (Date.now() - pollStart) / 1000;
      const sample = sampleProcessTree(child.pid, elapsedSec);
      if (sample) samples.push(sample);
    };
    takeSample();
    samplerTimer = setInterval(takeSample, opts.sampleIntervalSec * 1000);

    try {
      await waitForHealth(port, childAbort.signal);
    } catch (error) {
      assertChildRunning('during the health check');
      throw error;
    }
    assertChildRunning('during the health check');
    const bootMs = Date.now() - bootStart;
    let cookie;
    try {
      cookie = await signInLoopback(port, childAbort.signal);
    } catch (error) {
      assertChildRunning('during loopback sign-in');
      throw error;
    }
    assertChildRunning('during loopback sign-in');

    if (typeof opts.onServerReady === 'function') {
      await opts.onServerReady({ child, port, cookie });
    }

    pollStart = Date.now();
    const durationMs = opts.durationSec * 1000;
    takeSample();

    const routeRecords = [];
    await Promise.all([
      ...pollPlan.map((route) =>
        runPollLoop(route, port, cookie, pollStart, opts.durationSec, routeRecords, childAbort.signal)
      ),
      sleep(pollStart + durationMs - Date.now(), childAbort.signal),
    ]);
    takeSample();
    clearInterval(samplerTimer);
    samplerTimer = null;

    if (!unexpectedExit && !getChildExitStatus(child) && child.pid && !isPidAlive(child.pid)) {
      const waitDeadline = Date.now() + 500;
      while (!unexpectedExit && !getChildExitStatus(child) && Date.now() < waitDeadline) {
        await sleep(25);
      }
      if (!unexpectedExit && !getChildExitStatus(child)) {
        throw new Error(`Dashboard server died unexpectedly during run (pid=${child.pid}).`);
      }
    }
    assertChildRunning('during run');

    const memory = summarizeSamples(samples, opts.steadyAfterSec, {
      allowSteadyFallback: !opts.check,
    });
    const routes = summarizeRoutes(routeRecords, pollPlan);
    const routeFailures = evaluateRouteFailures(routes, opts.durationSec, pollPlan, {
      check: opts.check,
    });
    const sampleFailures = opts.check ? evaluateSampleFailures(memory, opts) : [];
    const threshold = thresholdConfig ? evaluateThresholds(memory, thresholdConfig) : null;
    const ok =
      routeFailures.length === 0 &&
      sampleFailures.length === 0 &&
      (!threshold || threshold.ok);

    const summary = {
      schemaVersion: 1,
      ok,
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
      sampleFailures,
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
  const pss = (value) => (value === null ? 'unavailable' : `${value} MiB`);
  const lines = [
    `[i] AAC memory budget run (${summary.durationSec}s, seed=${summary.seed}, samples=${m.sampleCount})`,
    `    timing: fixture=${(summary.timingMs.fixture / 1000).toFixed(1)}s boot=${(summary.timingMs.boot / 1000).toFixed(2)}s total=${(summary.timingMs.total / 1000).toFixed(1)}s`,
    `    server steady (t>=${m.steadyAfterSec}s): RSS=${m.steadyMedianRssMiB} MiB, PSS=${pss(m.steadyMedianPssMiB)}`,
    `    server peak:               RSS=${m.peakRssMiB} MiB, PSS=${pss(m.peakPssMiB)}`,
    `    children peak:             RSS=${m.childrenPeakRssMiB} MiB, PSS=${pss(m.childrenPeakPssMiB)} (max children=${m.maxChildren})`,
    `    total peak:                RSS=${m.totalPeakRssMiB} MiB, PSS=${pss(m.totalPeakPssMiB)}`,
    `    ${m.samplingNote}`,
  ];
  for (const [name, row] of Object.entries(summary.routes)) {
    lines.push(
      `    route ${name.padEnd(13)} ${String(row.ok).padStart(2)}/${String(row.calls).padEnd(2)} ok  bytes=${String(row.medianBytes).padStart(8)}  first=${String(row.firstMs).padStart(5)}ms  p50=${String(row.p50Ms).padStart(5)}ms  max=${String(row.maxMs).padStart(5)}ms`
    );
  }
  if (summary.routeFailures.length) {
    lines.push(`[X] Route failures: ${summary.routeFailures.join('; ')}`);
  }
  if (summary.sampleFailures?.length) {
    lines.push(`[X] Sample failures: ${summary.sampleFailures.join('; ')}`);
  }
  if (summary.threshold) {
    if (summary.threshold.ok) {
      lines.push('[OK] Memory within all configured limits:');
      for (const r of summary.threshold.results) {
        lines.push(
          `     ${r.metric}: ${r.measuredMiB} MiB <= ${r.limitMiB} MiB (${r.deltaMiB >= 0 ? '+' : ''}${r.deltaMiB} MiB)`
        );
      }
    } else {
      lines.push('[X] Memory budget EXCEEDED:');
      for (const v of summary.threshold.violations) {
        const kibDetail =
          typeof v.deltaKiB === 'number' && v.deltaKiB > 0 && v.deltaKiB < KIB_PER_MIB
            ? ` / +${v.deltaKiB} KiB`
            : '';
        lines.push(
          `     EXCEEDED ${v.metric}: ${v.measuredMiB} MiB > limit ${v.limitMiB} MiB (+${v.deltaMiB} MiB${kibDetail}, +${v.pctOver}%)`
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
  return summary.ok ? 0 : 1;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main(process.argv)
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      process.stderr.write(
        `[X] ${error instanceof Error ? error.stack || error.message : String(error)}\n`
      );
      process.exitCode = 1;
    });
}
