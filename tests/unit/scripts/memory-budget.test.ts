import { describe, expect, it } from 'bun:test';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  buildLegacySourceAccounts,
  buildSyntheticAccounts,
  createRng,
  generateMemoryFixture,
} from '../../../scripts/perf/memory-fixture.mjs';
import {
  POLL_PLAN,
  assertNoSymlinks,
  assertResolvedInside,
  buildIsolatedEnv,
  collectDescendants,
  evaluateRouteFailures,
  evaluateSampleFailures,
  evaluateThresholds,
  getChildExitStatus,
  loadThresholdConfig,
  parseProcStat,
  parseProcStatusRss,
  parseSmapsRollup,
  runMemoryBudget,
  summarizeRoutes,
  summarizeSamples,
  validateRunOptions,
  validateThresholdConfig,
} from '../../../scripts/perf/memory-budget.mjs';

const { scanText } = require('../../../scripts/personal-detail-guard.js');

const REPO_ROOT = path.resolve(import.meta.dir, '../../..');
const RUNNER_PATH = path.join(REPO_ROOT, 'scripts', 'perf', 'memory-budget.mjs');
const DEFAULT_CONFIG_PATH = path.join(REPO_ROOT, 'config', 'memory-budget.json');
const NODE_EXECUTABLE = Bun.which('node') ?? 'node';

function walkFiles(dir: string, base = dir, out: string[] = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walkFiles(full, base, out);
    else out.push(path.relative(base, full).split(path.sep).join('/'));
  }
  return out.sort();
}

function makeSample(
  tSec: number,
  serverRssKiB: number,
  childrenRssKiB = 0,
  serverPssKiB = serverRssKiB
) {
  return {
    tSec,
    serverRssKiB,
    serverPssKiB,
    childrenRssKiB,
    childrenPssKiB: childrenRssKiB,
    totalRssKiB: serverRssKiB + childrenRssKiB,
    totalPssKiB: serverPssKiB + childrenRssKiB,
    threads: 8,
    childrenCount: childrenRssKiB > 0 ? 1 : 0,
  };
}

/** Write a tiny Node HTTP server acting as a fake dashboard launcher for runner integration tests. */
function writeFakeDashboardScript(
  dir: string,
  filename: string,
  { failAfterFirst = false, truncatePath = '', startupMemory = false, analytics = 'ok' } = {}
) {
  const scriptPath = path.join(dir, filename);
  fs.writeFileSync(
    scriptPath,
    `import http from 'node:http';
import { gzipSync } from 'node:zlib';
const args = process.argv.slice(2);
const portIdx = args.indexOf('--port');
const port = portIdx >= 0 ? Number(args[portIdx + 1]) : 0;
const failAfterFirst = ${JSON.stringify(failAfterFirst)};
const truncatePath = ${JSON.stringify(truncatePath)};
const analytics = ${JSON.stringify(analytics)};
const hits = new Map();
const server = http.createServer((req, res) => {
  const url = req.url || '/';
  if (url === truncatePath) {
    res.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': '1024' });
    res.write('{', () => setTimeout(() => process.exit(17), 50));
    return;
  }
  if (url === '/api/health') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{"ok":true}');
    return;
  }
  if (url === '/api/auth/setup' && req.method === 'GET') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{"configured":false}');
    return;
  }
  if (url === '/api/auth/setup' && req.method === 'POST') {
    res.writeHead(201, {
      'Content-Type': 'application/json',
      'Set-Cookie': 'aac_session=fake-token; HttpOnly; Path=/',
    });
    res.end('{"ok":true}');
    return;
  }
  const count = (hits.get(url) ?? 0) + 1;
  hits.set(url, count);
  if (failAfterFirst && count > 1) {
    res.writeHead(500, { 'Content-Type': 'application/json' });
    res.end('{"error":"simulated_late_failure"}');
    return;
  }
  if (url.startsWith('/api/accounts/analytics?')) {
    const preset = new URL(url, 'http://localhost').searchParams.get('range');
    const body = gzipSync(JSON.stringify({
      stale: analytics === 'unavailable',
      range: { preset },
      history: { status: analytics, sampleCount: analytics === 'ok' ? 19 : 0 },
      activity: { status: 'ok', byDay: [{ date: '2026-11-25' }] },
    }));
    res.writeHead(analytics === 'http-error' ? 503 : 200, {
      'Content-Type': 'application/json',
      'Content-Encoding': 'gzip',
    });
    res.end(body);
    return;
  }
  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end('{"ok":true,"hit":' + count + '}');
});
if (${JSON.stringify(startupMemory)}) {
  let allocation = Buffer.alloc(160 * 1024 * 1024, 1);
  setTimeout(() => {
    if (allocation[0] !== 1) process.exit(18);
    allocation = null;
    global.gc();
    server.listen(port, '127.0.0.1');
  }, 800);
} else {
  server.listen(port, '127.0.0.1');
}
process.on('SIGTERM', () => server.close(() => process.exit(0)));
`
  );
  return scriptPath;
}

describe('memory budget fixture generator', () => {
  it('is byte-deterministic for a fixed seed and clock and contains no personal details', async () => {
    const rootA = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-fix-a-'));
    const rootB = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-fix-b-'));
    try {
      const fixedNow = Date.parse('2026-11-25T12:00:00Z');
      const smallOpts = {
        seed: 424242,
        now: fixedNow,
        writers: 16,
        runWriters: 4,
        runs: 110,
        registryEntries: 15,
        days: 3,
        accounts: 9,
        quotaSamplesPerDay: 2,
        activitySessions: 8,
        pricingProviders: 4,
        pricingModelsPerProvider: 3,
        skipStore: true,
      };
      const manifestA = await generateMemoryFixture(rootA, smallOpts);
      const manifestB = await generateMemoryFixture(rootB, smallOpts);

      expect(manifestA.counts).toEqual(manifestB.counts);
      expect(manifestA.counts.writers).toBe(16);
      expect(manifestA.counts.runs).toBe(110);
      expect(manifestA.counts.registryEntries).toBe(15);
      expect(manifestA.counts.providers).toBe(3);
      expect(manifestA.counts.accounts).toBe(9);
      expect(manifestA.counts.days).toBe(3);

      const filesA = walkFiles(rootA).filter((f) => f !== 'fixture-manifest.json');
      const filesB = walkFiles(rootB).filter((f) => f !== 'fixture-manifest.json');
      expect(filesA).toEqual(filesB);

      for (const rel of filesA) {
        const bytesA = fs.readFileSync(path.join(rootA, rel));
        const bytesB = fs.readFileSync(path.join(rootB, rel));
        if (
          rel.endsWith('.json') ||
          rel.endsWith('.jsonl') ||
          rel.endsWith('.md') ||
          rel.endsWith('.pub') ||
          rel.endsWith('.pem')
        ) {
          const textA = bytesA.toString('utf8');
          const textB = bytesB.toString('utf8');
          expect(textA.replaceAll(rootA, '<ROOT>')).toBe(textB.replaceAll(rootB, '<ROOT>'));
          if (rel.endsWith('.json') || rel.endsWith('.jsonl')) {
            expect(scanText(textA)).toEqual([]);
          }
        } else {
          expect(bytesA.equals(bytesB)).toBe(true);
        }
      }
    } finally {
      fs.rmSync(rootA, { recursive: true, force: true });
      fs.rmSync(rootB, { recursive: true, force: true });
    }
  });

  it('distributes 19 synthetic accounts cleanly across 3 providers with distinct identities', () => {
    const accounts = buildSyntheticAccounts(19);
    expect(accounts).toHaveLength(19);
    expect(new Set(accounts.map((a) => a.provider)).size).toBe(3);
    expect(new Set(accounts.map((a) => a.identity)).size).toBe(19);
    expect(accounts.every((a) => a.email.endsWith('@example.invalid'))).toBe(true);
  });

  it('builds the legacy <provider>:usage source rows from the provider table with derive-matching identities', () => {
    const table = JSON.parse(
      fs.readFileSync(path.join(REPO_ROOT, 'config', 'tables', 'providers.json'), 'utf8')
    );
    const accounts = buildLegacySourceAccounts();
    expect(accounts.map((a) => a.id)).toEqual(table.additionalProviders.map((p: string) => `${p}:usage`));
    expect(new Set(accounts.map((a) => a.provider))).toEqual(new Set(table.additionalProviders));
    expect(accounts.every((a) => a.platform === 'ubuntu')).toBe(true);
    for (const account of accounts) {
      const expected = createHash('sha256')
        .update(JSON.stringify([account.provider, account.id, null]))
        .digest('hex');
      expect(account.identity).toBe(expected);
    }
  });

  it('produces reproducible sequences from createRng', () => {
    const a = createRng(99);
    const b = createRng(99);
    const c = createRng(100);
    const seqA = Array.from({ length: 10 }, () => a.next());
    const seqB = Array.from({ length: 10 }, () => b.next());
    const seqC = Array.from({ length: 10 }, () => c.next());
    expect(seqA).toEqual(seqB);
    expect(seqA).not.toEqual(seqC);
  });
});

describe('memory budget sampler and threshold evaluator', () => {
  it('parses /proc smaps_rollup and stat and collects descendants', () => {
    const rollup = parseSmapsRollup(
      [
        '55b8a000-7ffd0000 ---p 00000000 00:00 0 [rollup]',
        'Rss:              256000 kB',
        'Pss:              245760 kB',
        'Pss_Anon:         220160 kB',
        'Pss_File:          25600 kB',
        'Swap:               1024 kB',
      ].join('\n')
    );
    expect(rollup).toEqual({
      rssKiB: 256000,
      pssKiB: 245760,
      pssAnonKiB: 220160,
      pssFileKiB: 25600,
      swapKiB: 1024,
      pssAvailable: true,
    });

    const stat = parseProcStat(
      '1234 (ai-account (1)) S 42 1234 1234 0 -1 4194304 1 0 0 0 10 20 0 0 20 0 14 0 100 0 0'
    );
    expect(stat).toEqual({ ppid: 42, threads: 14 });

    const parents = new Map([
      [100, 1],
      [101, 100],
      [102, 100],
      [103, 101],
      [200, 1],
    ]);
    expect(collectDescendants(100, parents).sort((x, y) => x - y)).toEqual([101, 102, 103]);
  });

  it('computes steady median after minute 2 and peak metrics and fails over threshold', () => {
    const samples = [
      makeSample(0, 100 * 1024, 0, 90 * 1024),
      makeSample(60, 400 * 1024, 50 * 1024, 380 * 1024),
      makeSample(120, 200 * 1024, 10 * 1024, 190 * 1024),
      makeSample(180, 210 * 1024, 0, 200 * 1024),
      makeSample(240, 220 * 1024, 0, 210 * 1024),
    ];
    const summary = summarizeSamples(samples, 120);
    expect(summary.steadyMedianRssMiB).toBe(210);
    expect(summary.steadyMedianPssMiB).toBe(200);
    expect(summary.peakRssMiB).toBe(400);
    expect(summary.peakPssMiB).toBe(380);
    expect(summary.childrenPeakRssMiB).toBe(50);
    expect(summary.totalPeakRssMiB).toBe(450);

    const pass = evaluateThresholds(summary, {
      limits: { steadyRssMiB: 220, peakRssMiB: 410, childrenPeakRssMiB: 60 },
    });
    expect(pass.ok).toBe(true);
    expect(pass.violations).toEqual([]);

    const fail = evaluateThresholds(summary, {
      limits: { steadyRssMiB: 200, peakRssMiB: 350, childrenPeakRssMiB: 60 },
    });
    expect(fail.ok).toBe(false);
    expect(fail.violations.map((v) => v.metric)).toEqual(['steadyRssMiB', 'peakRssMiB']);
    expect(fail.violations[0].deltaMiB).toBe(10);
    expect(fail.violations[1].deltaMiB).toBe(50);
  });

  it('rejects misspelled, missing, non-finite, and unknown limit keys before fixture generation', () => {
    const summary = summarizeSamples(
      [
        makeSample(0, 700 * 1024),
        makeSample(120, 700 * 1024),
        makeSample(240, 900 * 1024, 80 * 1024),
      ],
      120
    );

    const misspelled = {
      schemaVersion: 1,
      limits: { steadyRssMib: 320, peakRssMib: 651, childrenPeakRssMib: 16 },
    };
    expect(() => validateThresholdConfig(misspelled)).toThrow(/unknown limit key/i);
    expect(() => evaluateThresholds(summary, misspelled)).toThrow(/unknown limit key/i);

    expect(() =>
      validateThresholdConfig({
        schemaVersion: 1,
        limits: { steadyRssMiB: 320, peakRssMiB: 651, childrenPeakRssMiB: 16, extraLimit: 5 },
      })
    ).toThrow(/unknown limit key/i);

    expect(() =>
      validateThresholdConfig({
        schemaVersion: 1,
        limits: { steadyRssMiB: 320, peakRssMiB: 651 },
      })
    ).toThrow(/missing required limit "childrenPeakRssMiB"/i);

    for (const bad of [null, Number.NaN, Number.POSITIVE_INFINITY, -1, '320']) {
      expect(() =>
        validateThresholdConfig({
          schemaVersion: 1,
          limits: { steadyRssMiB: bad, peakRssMiB: 651, childrenPeakRssMiB: 16 },
        })
      ).toThrow(/finite non-negative number/i);
    }

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-badcfg-'));
    try {
      const badCfgPath = path.join(tmpDir, 'bad-budget.json');
      fs.writeFileSync(badCfgPath, `${JSON.stringify(misspelled, null, 2)}\n`);
      expect(() => loadThresholdConfig(badCfgPath)).toThrow(/unknown limit key/i);

      // CLI must fail immediately during config validation before fixture generation.
      const res = spawnSync(
        process.execPath,
        [RUNNER_PATH, '--config', badCfgPath, '--fixture-dir', path.join(tmpDir, 'does-not-exist')],
        { encoding: 'utf8', timeout: 10_000 }
      );
      expect(res.status).not.toBe(0);
      expect(res.stderr).toMatch(/unknown limit key/i);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('compares unrounded values so 4 KiB over a limit fails while exact limit passes', () => {
    const committedConfig = loadThresholdConfig(DEFAULT_CONFIG_PATH);

    const exactSamples = [
      makeSample(0, 200 * 1024, 0),
      makeSample(60, 651 * 1024, 16 * 1024),
      makeSample(120, 320 * 1024, 0),
      makeSample(180, 320 * 1024, 0),
      makeSample(240, 320 * 1024, 0),
    ];
    const exactSummary = summarizeSamples(exactSamples, 120);
    const exactCheck = evaluateThresholds(exactSummary, committedConfig);
    expect(exactCheck.ok).toBe(true);
    expect(exactCheck.violations).toEqual([]);

    // 4 KiB over peak (651 MiB + 4 KiB) and 4 KiB over children peak (16 MiB + 4 KiB)
    // and 4 KiB over steady (320 MiB + 4 KiB) would all round down to the limit at 2 decimals.
    const over4KiBSamples = [
      makeSample(0, 200 * 1024, 0),
      makeSample(60, 651 * 1024 + 4, 16 * 1024 + 4),
      makeSample(120, 320 * 1024 + 4, 0),
      makeSample(180, 320 * 1024 + 4, 0),
      makeSample(240, 320 * 1024 + 4, 0),
    ];
    const overSummary = summarizeSamples(over4KiBSamples, 120);
    expect(overSummary.peakRssMiB).toBe(651);
    expect(overSummary.childrenPeakRssMiB).toBe(16);
    expect(overSummary.steadyMedianRssMiB).toBe(320);

    const overCheck = evaluateThresholds(overSummary, committedConfig);
    expect(overCheck.ok).toBe(false);
    expect(overCheck.violations.map((v) => v.metric)).toEqual([
      'steadyRssMiB',
      'peakRssMiB',
      'childrenPeakRssMiB',
    ]);
    for (const v of overCheck.violations) {
      expect(v.deltaKiB).toBe(4);
      expect(v.deltaMiB).toBeGreaterThan(0);
      expect(v.measuredMiB).toBeGreaterThan(v.limitMiB);
    }
  });

  it('refuses steady fallback and insufficient steady samples in check mode while allowing measure-only fallback', () => {
    const earlyOnly = [makeSample(0, 120 * 1024), makeSample(2, 130 * 1024)];
    expect(() => summarizeSamples(earlyOnly, 120)).toThrow(/steady window/i);

    const fallbackSummary = summarizeSamples(earlyOnly, 120, { allowSteadyFallback: true });
    expect(fallbackSummary.steadySampleCount).toBe(0);
    expect(fallbackSummary.steadyUsedFallback).toBe(true);
    expect(() =>
      evaluateThresholds(fallbackSummary, loadThresholdConfig(DEFAULT_CONFIG_PATH))
    ).toThrow(/steady-window samples/i);

    const sampleFailures = evaluateSampleFailures(fallbackSummary, {
      durationSec: 300,
      steadyAfterSec: 120,
      sampleIntervalSec: 2,
    });
    expect(sampleFailures.length).toBeGreaterThanOrEqual(2);

    // Too few steady samples (killed shortly after minute 2) also fails check mode.
    const shortSteady = [
      makeSample(0, 100 * 1024),
      makeSample(120, 200 * 1024),
      makeSample(122, 205 * 1024),
      makeSample(124, 210 * 1024),
    ];
    const shortSummary = summarizeSamples(shortSteady, 120);
    const shortFailures = evaluateSampleFailures(shortSummary, {
      durationSec: 300,
      steadyAfterSec: 120,
      sampleIntervalSec: 2,
    });
    expect(shortFailures.some((msg) => msg.includes('Insufficient steady-window samples'))).toBe(
      true
    );
    expect(shortFailures.some((msg) => msg.includes('ended too early'))).toBe(true);
  });

  it('fails zero-length runs and rejects later poll failures across the measurement window', () => {
    expect(() =>
      validateRunOptions({ durationSec: 0, steadyAfterSec: 120, sampleIntervalSec: 2, check: true })
    ).toThrow(/durationSec/i);
    expect(() =>
      validateRunOptions({
        durationSec: 0,
        steadyAfterSec: 120,
        sampleIntervalSec: 2,
        check: false,
      })
    ).toThrow(/durationSec/i);
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 120,
        sampleIntervalSec: 2,
        check: true,
      })
    ).toThrow(/steadyAfterSec/i);
    // Explicit measure-only mode allows short runs whose steadyAfterSec exceeds durationSec.
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 120,
        sampleIntervalSec: 2,
        check: false,
      })
    ).not.toThrow();

    // CLI zero-length check run fails immediately before fixture generation.
    const zeroCli = spawnSync(process.execPath, [RUNNER_PATH, '--duration', '0'], {
      encoding: 'utf8',
      timeout: 10_000,
    });
    expect(zeroCli.status).not.toBe(0);
    expect(zeroCli.stderr).toMatch(/durationSec/i);

    // Zero calls across all routes must fail.
    const zeroRoutes = summarizeRoutes([]);
    const zeroFailures = evaluateRouteFailures(zeroRoutes, 300);
    expect(zeroFailures).toHaveLength(POLL_PLAN.length);

    // 7 initial HTTP 200s followed by 54 later failures across a 5-minute schedule must fail.
    const lateFailureRecords = [];
    for (const route of POLL_PLAN) {
      let idx = 0;
      for (let t = route.offsetSec; t < 300; t += route.periodSec) {
        lateFailureRecords.push({
          tSec: t,
          route: route.name,
          status: idx === 0 ? 200 : 500,
          bytes: idx === 0 ? 1024 : 0,
          encoding: '',
          ms: 5,
        });
        idx += 1;
      }
    }
    const lateSummary = summarizeRoutes(lateFailureRecords);
    const lateFailures = evaluateRouteFailures(lateSummary, 300);
    expect(lateFailures).toHaveLength(6);
    expect(lateFailures.every((msg) => msg.includes('polls failed'))).toBe(true);

    // Full 5-minute healthy schedule passes cleanly.
    const healthyRecords = [];
    for (const route of POLL_PLAN) {
      for (let t = route.offsetSec; t < 300; t += route.periodSec) {
        healthyRecords.push({
          tSec: t,
          route: route.name,
          status: 200,
          bytes: 1024,
          encoding: '',
          ms: 5,
        });
      }
    }
    expect(evaluateRouteFailures(summarizeRoutes(healthyRecords), 300)).toEqual([]);
  });

  it('detects a SIGKILLed real child process via signalCode even when exitCode is null', async () => {
    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000);'], {
      stdio: 'ignore',
    });
    const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
    child.kill('SIGKILL');
    await exited;

    expect(child.exitCode).toBeNull();
    expect(child.signalCode).toBe('SIGKILL');
    expect(getChildExitStatus(child)).toEqual({
      exitCode: null,
      signalCode: 'SIGKILL',
      reason: 'signal=SIGKILL',
    });
  });

  it('requires the last scheduled successful poll for every route', () => {
    for (const missingRoute of POLL_PLAN) {
      const durationSec = 600;
      const records = POLL_PLAN.flatMap((route) => {
        const polls = [];
        for (let t = route.offsetSec; t < durationSec; t += route.periodSec) {
          polls.push({ route: route.name, status: 200, bytes: 1024, ms: 5 });
        }
        if (route === missingRoute) polls.pop();
        return polls;
      });
      expect(evaluateRouteFailures(summarizeRoutes(records), durationSec)).toEqual([
        expect.stringContaining(`${missingRoute.name}: only `),
      ]);
    }
  });

  it.skipIf(process.platform !== 'linux')(
    'exits 1 when the dashboard exits after sending partial sign-in headers and body',
    () => {
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-truncated-'));
      try {
        const script = writeFakeDashboardScript(tmpDir, 'dashboard.mjs', {
          truncatePath: '/api/auth/setup',
        });
        // Use the same promise-to-exit-code handling as the CLI, with a fake launcher.
        const result = spawnSync(
          NODE_EXECUTABLE,
          [
            '--input-type=module',
            '--eval',
            `import { runMemoryBudget } from ${JSON.stringify(RUNNER_PATH)};
runMemoryBudget({
  durationSec: 1, steadyAfterSec: 0, sampleIntervalSec: 0.2, check: false,
  launcherPath: process.execPath,
  launcherArgs: ({ port }) => [${JSON.stringify(script)}, '--port', String(port)],
}).then((summary) => {
  process.exitCode = summary.ok ? 0 : 1;
}).catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});`,
          ],
          { encoding: 'utf8', timeout: 4000, env: { ...process.env, TMPDIR: tmpDir } }
        );
        expect(result.error).toBeUndefined();
        expect(result.status).toBe(1);
        expect(result.stderr).toMatch(/\/api\/auth\/setup|during loopback sign-in/);
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    }
  );

  it.skipIf(process.platform !== 'linux')(
    'includes a released startup allocation in peak RSS and excludes it from steady RSS',
    async () => {
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-startup-'));
      try {
        const script = writeFakeDashboardScript(tmpDir, 'dashboard.mjs', { startupMemory: true });
        const configPath = path.join(tmpDir, 'budget.json');
        fs.writeFileSync(
          configPath,
          JSON.stringify({
            schemaVersion: 1,
            limits: { steadyRssMiB: 128, peakRssMiB: 128, childrenPeakRssMiB: 16 },
          })
        );
        const summary = await runMemoryBudget({
          durationSec: 1,
          steadyAfterSec: 0,
          sampleIntervalSec: 0.2,
          check: true,
          configPath,
          pollPlan: [{ name: 'status', path: '/api/status', periodSec: 2, offsetSec: 0 }],
          launcherPath: NODE_EXECUTABLE,
          launcherArgs: ({ port }) => ['--expose-gc', script, '--port', String(port)],
        });

        expect(summary.memory.steadyMedianRssMiB).toBeLessThan(128);
        expect(summary.memory.peakRssMiB).toBeGreaterThan(160);
        expect(summary.threshold?.violations.map((v) => v.metric)).toEqual(['peakRssMiB']);
        expect(summary.ok).toBe(false);
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    }
  );

  it.skipIf(process.platform !== 'linux')(
    'requires available Analytics data even when the HTTP response is 200 and compressed',
    async () => {
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-analytics-'));
      const route = POLL_PLAN.find((item) => item.name === 'analytics30d')!;
      try {
        for (const [analytics, expectedOk] of [
          ['ok', true],
          ['http-error', false],
          ['unavailable', false],
        ] as const) {
          const script = writeFakeDashboardScript(tmpDir, `${analytics}.mjs`, { analytics });
          const summary = await runMemoryBudget({
            durationSec: 0.3,
            steadyAfterSec: 0,
            sampleIntervalSec: 0.2,
            check: false,
            pollPlan: [{ ...route, offsetSec: 0 }],
            launcherPath: process.execPath,
            launcherArgs: ({ port }) => [script, '--port', String(port)],
          });

          expect(summary.ok).toBe(expectedOk);
          expect(summary.routes.analytics30d).toMatchObject({
            calls: 1,
            ok: Number(expectedOk),
            failed: Number(!expectedOk),
          });
        }
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    }
  );

  it.skipIf(process.platform !== 'linux')(
    'fails runMemoryBudget on a SIGKILLed child, fails on later poll errors, and passes a healthy run',
    async () => {
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-e2e-'));
      const fastPollPlan = [
        { name: 'status', path: '/api/status', periodSec: 0.4, offsetSec: 0 },
        { name: 'accounts', path: '/api/accounts/dashboard', periodSec: 0.5, offsetSec: 0.1 },
      ];
      const generousConfigPath = path.join(tmpDir, 'budget.json');
      fs.writeFileSync(
        generousConfigPath,
        `${JSON.stringify(
          {
            schemaVersion: 1,
            limits: { steadyRssMiB: 2048, peakRssMiB: 2048, childrenPeakRssMiB: 512 },
          },
          null,
          2
        )}\n`
      );
      try {
        // 1. SIGKILLed dashboard child during measurement must reject with signal=SIGKILL.
        const okScript = writeFakeDashboardScript(tmpDir, 'ok-dashboard.mjs', {
          failAfterFirst: false,
        });
        let killTimer: ReturnType<typeof setTimeout> | null = null;
        await expect(
          runMemoryBudget({
            durationSec: 4,
            steadyAfterSec: 1.5,
            sampleIntervalSec: 0.25,
            seed: 1,
            configPath: generousConfigPath,
            check: true,
            pollPlan: fastPollPlan,
            launcherPath: process.execPath,
            launcherArgs: ({ port }) => [okScript, '--port', String(port)],
            onServerReady: ({ child }) => {
              killTimer = setTimeout(() => child.kill('SIGKILL'), 350);
            },
          })
        ).rejects.toThrow(/signal=SIGKILL/);
        if (killTimer) clearTimeout(killTimer);

        // 2. Later poll failures after initial 200s must mark the run failed.
        const flakyScript = writeFakeDashboardScript(tmpDir, 'flaky-dashboard.mjs', {
          failAfterFirst: true,
        });
        const flakySummary = await runMemoryBudget({
          durationSec: 1.8,
          steadyAfterSec: 0.5,
          sampleIntervalSec: 0.2,
          seed: 1,
          configPath: generousConfigPath,
          check: true,
          pollPlan: fastPollPlan,
          launcherPath: process.execPath,
          launcherArgs: ({ port }) => [flakyScript, '--port', String(port)],
        });
        expect(flakySummary.ok).toBe(false);
        expect(flakySummary.routeFailures.length).toBeGreaterThanOrEqual(2);
        expect(flakySummary.routeFailures.every((msg) => msg.includes('polls failed'))).toBe(true);

        // 3. Healthy child with 100% 200s and full steady window passes.
        const healthySummary = await runMemoryBudget({
          durationSec: 1.8,
          steadyAfterSec: 0.5,
          sampleIntervalSec: 0.2,
          seed: 1,
          configPath: generousConfigPath,
          check: true,
          pollPlan: fastPollPlan,
          launcherPath: process.execPath,
          launcherArgs: ({ port }) => [okScript, '--port', String(port)],
        });
        expect(healthySummary.ok).toBe(true);
        expect(healthySummary.routeFailures).toEqual([]);
        expect(healthySummary.sampleFailures).toEqual([]);
        expect(healthySummary.memory.steadyUsedFallback).toBe(false);
        expect(healthySummary.memory.steadySampleCount).toBeGreaterThanOrEqual(3);
        expect(healthySummary.threshold?.ok).toBe(true);
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    }
  );

  it('isolates every home variable and polls the required routes on schedule', () => {
    const env = buildIsolatedEnv({
      home: '/tmp/aac-test-home',
      ccsDir: '/tmp/aac-test-home/.ccs',
      reportsDir: '/tmp/aac-test-reports',
      registryDir: '/tmp/aac-test-reports/pm-mail-registry',
      stubsDir: '/tmp/aac-test-stubs',
      xdgRuntimeDir: '/tmp/aac-test-xdg',
    });
    for (const key of [
      'HOME',
      'CCS_HOME',
      'USERPROFILE',
      'CODEX_HOME',
      'CLAUDE_CONFIG_DIR',
      'XDG_CONFIG_HOME',
    ]) {
      expect(env[key]).toStartWith('/tmp/aac-test-home');
    }
    expect(env.AAC_STATUS_DOORBELL).toBe('off');
    expect(env.AAC_AGENT_TRANSPORT).toBe('off');
    expect(env.AAC_STORE_SHARE_BACKUP_DIR).toBe('off');

    const byName = Object.fromEntries(POLL_PLAN.map((r) => [r.name, r.periodSec]));
    expect(byName).toEqual({
      status: 10,
      scoreboard: 30,
      analytics7d: 60,
      analytics30d: 300,
      'home-mac': 60,
      'home-win': 60,
      accounts: 60,
    });
  });

  it('tracks a valid config/memory-budget.json threshold file', () => {
    const config = loadThresholdConfig(DEFAULT_CONFIG_PATH);
    expect(config.schemaVersion).toBe(1);
    expect(typeof config.limits.steadyRssMiB).toBe('number');
    expect(config.limits.steadyRssMiB).toBeGreaterThan(0);
    expect(typeof config.limits.peakRssMiB).toBe('number');
    expect(config.limits.peakRssMiB).toBeGreaterThanOrEqual(config.limits.steadyRssMiB);
    expect(typeof config.limits.childrenPeakRssMiB).toBe('number');
    expect(config.limits.childrenPeakRssMiB).toBeGreaterThanOrEqual(0);
  });
});

describe('memory-budget tool-batch-1126 fixes', () => {
  it('RSS fallback marks PSS unavailable instead of reporting RSS as PSS', () => {
    const fallback = parseProcStatusRss('VmRSS: 78156 kB\nVmSwap: 0 kB\n');
    expect(fallback.rssKiB).toBe(78156);
    expect(fallback.pssAvailable).toBe(false);
    expect(fallback.pssKiB).toBeNull();
    // RSS must not be copied into PSS.
    expect(fallback.pssKiB).not.toBe(78156);
  });

  it('summarize and threshold reject PSS limits without valid PSS coverage', () => {
    const sample = {
      tSec: 130,
      serverRssKiB: 1000,
      serverPssKiB: null,
      threads: 1,
      childrenCount: 1,
      childrenRssKiB: 200,
      childrenPssKiB: null,
      totalRssKiB: 1200,
      totalPssKiB: null,
    };
    const mem = summarizeSamples([sample], 120);
    const rssLimits = { steadyRssMiB: 100, peakRssMiB: 100, childrenPeakRssMiB: 100 };
    const rssConfig = { schemaVersion: 1, limits: rssLimits };
    const serverConfig = { schemaVersion: 1, limits: { ...rssLimits, steadyPssMiB: 50 } };
    const totalConfig = { schemaVersion: 1, limits: { ...rssLimits, totalPeakPssMiB: 50 } };
    expect(mem.pssAvailable).toBe(false);
    expect(mem.steadyMedianPssKiB).toBeNull();
    expect(() => evaluateThresholds(mem, serverConfig)).toThrow(/PSS coverage/);
    expect(evaluateThresholds(mem, rssConfig).ok).toBe(true);

    // A fallback in the child invalidates the total while the measured server PSS is usable.
    const serverMeasured = summarizeSamples([{ ...sample, serverPssKiB: 900 }], 120);
    expect(serverMeasured.pssAvailable).toBe(false);
    expect(evaluateThresholds(serverMeasured, serverConfig).ok).toBe(true);
    expect(() => evaluateThresholds(serverMeasured, totalConfig)).toThrow(/PSS coverage/);
  });

  it('rejects sub-200 ms sample intervals consistently', () => {
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 5,
        sampleIntervalSec: 0.05,
        check: true,
      })
    ).toThrow(/>= 0.2/);
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 5,
        sampleIntervalSec: 0.19,
        check: true,
      })
    ).toThrow(/>= 0.2/);
    // 0.2 is the supported minimum and passes when the window is sufficient.
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 5,
        sampleIntervalSec: 0.2,
        check: true,
      })
    ).not.toThrow();
    // Also rejects in measure-only mode (the timer clamp applies either way).
    expect(() =>
      validateRunOptions({
        durationSec: 10,
        steadyAfterSec: 5,
        sampleIntervalSec: 0.05,
        check: false,
      })
    ).toThrow(/>= 0.2/);
  });

  it('refuses symlinked fixture inputs before starting a child', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mem-link-'));
    try {
      const fixture = path.join(root, 'fixture');
      const outside = path.join(root, 'outside');
      const home = path.join(fixture, 'home');
      fs.mkdirSync(home, { recursive: true });
      fs.mkdirSync(outside);
      const linked = path.join(home, '.ccs');
      fs.symlinkSync(outside, linked, 'dir');
      expect(() => assertNoSymlinks(fixture)).toThrow(/symlink/);
      expect(() => assertResolvedInside(fixture, linked)).toThrow(/escapes the fixture/);
      expect(() => assertResolvedInside(fixture, home)).not.toThrow();
      await expect(generateMemoryFixture(linked)).rejects.toThrow(/symlink/);
      await expect(
        runMemoryBudget({
          durationSec: 1,
          steadyAfterSec: 0,
          sampleIntervalSec: 0.2,
          check: false,
          fixtureDir: fixture,
          launcherPath: process.execPath,
          launcherArgs: () => {
            throw new Error('the launcher must not start');
          },
        })
      ).rejects.toThrow(/symlink/);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('documents the short-lived helper sampling limit in the summary', () => {
    const mem = summarizeSamples(
      [
        {
          tSec: 0,
          serverRssKiB: 100,
          serverPssKiB: 90,
          pssAvailable: true,
          threads: 1,
          childrenCount: 0,
          childrenRssKiB: 0,
          childrenPssKiB: 0,
          totalRssKiB: 100,
          totalPssKiB: 90,
        },
        {
          tSec: 130,
          serverRssKiB: 100,
          serverPssKiB: 90,
          pssAvailable: true,
          threads: 1,
          childrenCount: 0,
          childrenRssKiB: 0,
          childrenPssKiB: 0,
          totalRssKiB: 100,
          totalPssKiB: 90,
        },
      ],
      120
    );
    expect(mem.samplingNote).toMatch(/missed|between samples/i);
  });
});
