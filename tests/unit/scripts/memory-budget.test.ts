import { describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  buildSyntheticAccounts,
  createRng,
  generateMemoryFixture,
} from '../../../scripts/perf/memory-fixture.mjs';
import {
  POLL_PLAN,
  buildIsolatedEnv,
  collectDescendants,
  evaluateThresholds,
  parseProcStat,
  parseSmapsRollup,
  summarizeSamples,
} from '../../../scripts/perf/memory-budget.mjs';

const { scanText } = require('../../../scripts/personal-detail-guard.js');

function walkFiles(dir, base = dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walkFiles(full, base, out);
    else out.push(path.relative(base, full).split(path.sep).join('/'));
  }
  return out.sort();
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
        if (rel.endsWith('.json') || rel.endsWith('.jsonl') || rel.endsWith('.md') || rel.endsWith('.pub') || rel.endsWith('.pem')) {
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
      { tSec: 0, serverRssKiB: 100 * 1024, serverPssKiB: 90 * 1024, childrenRssKiB: 0, childrenPssKiB: 0, totalRssKiB: 100 * 1024, totalPssKiB: 90 * 1024, threads: 8, childrenCount: 0 },
      { tSec: 60, serverRssKiB: 400 * 1024, serverPssKiB: 380 * 1024, childrenRssKiB: 50 * 1024, childrenPssKiB: 45 * 1024, totalRssKiB: 450 * 1024, totalPssKiB: 425 * 1024, threads: 12, childrenCount: 1 },
      { tSec: 120, serverRssKiB: 200 * 1024, serverPssKiB: 190 * 1024, childrenRssKiB: 10 * 1024, childrenPssKiB: 8 * 1024, totalRssKiB: 210 * 1024, totalPssKiB: 198 * 1024, threads: 10, childrenCount: 1 },
      { tSec: 180, serverRssKiB: 210 * 1024, serverPssKiB: 200 * 1024, childrenRssKiB: 0, childrenPssKiB: 0, totalRssKiB: 210 * 1024, totalPssKiB: 200 * 1024, threads: 10, childrenCount: 0 },
      { tSec: 240, serverRssKiB: 220 * 1024, serverPssKiB: 210 * 1024, childrenRssKiB: 0, childrenPssKiB: 0, totalRssKiB: 220 * 1024, totalPssKiB: 210 * 1024, threads: 10, childrenCount: 0 },
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

  it('isolates every home variable and polls the required routes on schedule', () => {
    const env = buildIsolatedEnv({
      home: '/tmp/aac-test-home',
      ccsDir: '/tmp/aac-test-home/.ccs',
      reportsDir: '/tmp/aac-test-reports',
      registryDir: '/tmp/aac-test-reports/pm-mail-registry',
      stubsDir: '/tmp/aac-test-stubs',
      xdgRuntimeDir: '/tmp/aac-test-xdg',
    });
    for (const key of ['HOME', 'CCS_HOME', 'USERPROFILE', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'XDG_CONFIG_HOME']) {
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
    const configPath = path.resolve(import.meta.dir, '../../../config/memory-budget.json');
    const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
    expect(config.schemaVersion).toBe(1);
    expect(typeof config.limits.steadyRssMiB).toBe('number');
    expect(config.limits.steadyRssMiB).toBeGreaterThan(0);
    expect(typeof config.limits.peakRssMiB).toBe('number');
    expect(config.limits.peakRssMiB).toBeGreaterThanOrEqual(config.limits.steadyRssMiB);
  });
});
