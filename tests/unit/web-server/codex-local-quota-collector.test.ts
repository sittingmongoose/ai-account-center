/**
 * Tests for the Codex local quota collector (zero network).
 *
 * Uses a temp fixture rollout-*.jsonl read via the real Bun.spawn(['tail', ...])
 * default impl (macOS-safe) plus injected fs seams for the directory walk.
 */

import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { getCodexLocalQuota } from '../../../src/web-server/usage/codex-local-quota-collector';

let tmpDir: string;
let codexHome: string;
let sessionsDir: string;

function tokenCountLine(rateLimits: unknown): string {
  return JSON.stringify({
    timestamp: '2026-06-09T14:36:48.896Z',
    type: 'event_msg',
    payload: { type: 'token_count', info: {}, rate_limits: rateLimits },
  });
}

function writeRollout(sessions: string, name: string, lines: string[]): string {
  const day = path.join(sessions, '2026', '06', '09');
  fs.mkdirSync(day, { recursive: true });
  const file = path.join(day, name);
  fs.writeFileSync(file, lines.join('\n') + '\n');
  return file;
}

/**
 * Each test gets its OWN codex home so the multi-session scan never bleeds a
 * fixture from another test (the scan walks ALL recent sessions, not just one).
 */
function freshHome(slug: string): { env: NodeJS.ProcessEnv; sessions: string } {
  const home = path.join(tmpDir, `.codex-${slug}`);
  const sessions = path.join(home, 'sessions');
  fs.mkdirSync(sessions, { recursive: true });
  return { env: { CODEX_HOME: home }, sessions };
}

beforeAll(() => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-local-quota-'));
  codexHome = path.join(tmpDir, '.codex');
  sessionsDir = path.join(codexHome, 'sessions');
  fs.mkdirSync(sessionsDir, { recursive: true });
});

afterAll(() => {
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

describe('getCodexLocalQuota', () => {
  it('parses the last non-null rate_limits into a normalized quota', async () => {
    const { env, sessions } = freshHome('parse');
    writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [
      tokenCountLine(null),
      tokenCountLine({
        primary: { used_percent: 0.0, window_minutes: 300, resets_at: 1781033803 },
        secondary: { used_percent: 48.0, window_minutes: 10080, resets_at: 1781192122 },
        plan_type: 'pro',
      }),
    ]);

    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    expect(quota).not.toBeNull();
    // min(100-0, 100-48) = 52
    expect(quota?.quotaPercentage).toBe(52);
    expect(quota?.tier).toBe('pro');
    // soonest reset = min(1781033803, 1781192122) -> primary
    expect(quota?.nextReset).toBe(new Date(1781033803 * 1000).toISOString());
    expect(quota?.stale).toBe(false);
    expect(quota?.staleAsOf).toBeNull();
  });

  it('surfaces per-window detail incl. window_minutes (300 / 10080)', async () => {
    const { env, sessions } = freshHome('windows');
    writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [
      tokenCountLine({
        primary: { used_percent: 19.0, window_minutes: 300, resets_at: 1781033803 },
        secondary: { used_percent: 30.0, window_minutes: 10080, resets_at: 1781192122 },
        plan_type: 'pro',
      }),
    ]);

    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    expect(quota?.windows).toHaveLength(2);

    const five = quota?.windows.find((w) => w.key === 'five_hour');
    expect(five?.label).toBe('5h');
    expect(five?.usedPercent).toBe(19);
    expect(five?.remainingPercent).toBe(81);
    expect(five?.windowMinutes).toBe(300);
    expect(five?.resetAt).toBe(new Date(1781033803 * 1000).toISOString());

    const week = quota?.windows.find((w) => w.key === 'seven_day');
    expect(week?.label).toBe('week');
    expect(week?.usedPercent).toBe(30);
    expect(week?.remainingPercent).toBe(70);
    expect(week?.windowMinutes).toBe(10080);
    expect(week?.resetAt).toBe(new Date(1781192122 * 1000).toISOString());
  });

  it('scans an OLDER session when the newest is exec-mode (rate_limits:null)', async () => {
    const { env, sessions } = freshHome('fallback');
    // Older interactive session carries real quota.
    writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [
      tokenCountLine({
        primary: { used_percent: 0.0, window_minutes: 300, resets_at: 1781033803 },
        secondary: { used_percent: 48.0, window_minutes: 10080, resets_at: 1781192122 },
        plan_type: 'pro',
      }),
    ]);
    // Newest session is exec-mode: only null rate_limits.
    writeRollout(sessions, 'rollout-2026-06-09T11-00-00-bbbb.jsonl', [
      tokenCountLine(null),
      tokenCountLine(null),
    ]);

    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    // Skips the null-newest file, reads the older file's rate_limits.
    expect(quota).not.toBeNull();
    expect(quota?.quotaPercentage).toBe(52);
    expect(quota?.tier).toBe('pro');
  });

  it('returns null when NO scanned session carries rate_limits (no fake row)', async () => {
    const { env, sessions } = freshHome('all-null');
    writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [tokenCountLine(null)]);
    writeRollout(sessions, 'rollout-2026-06-09T11-00-00-bbbb.jsonl', [
      tokenCountLine(null),
      tokenCountLine(null),
    ]);
    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    expect(quota).toBeNull();
  });

  it('flags stale from the SOURCE file mtime and sets staleAsOf', async () => {
    const { env, sessions } = freshHome('stale');
    // Newest is exec-mode (null); the data comes from the older file, so stale
    // must reflect the OLDER file's mtime, not the newest's.
    const sourceFile = writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [
      tokenCountLine({
        primary: { used_percent: 10, window_minutes: 300, resets_at: 1781033803 },
        secondary: { used_percent: 5, window_minutes: 10080, resets_at: 1781192122 },
        plan_type: 'plus',
      }),
    ]);
    writeRollout(sessions, 'rollout-2026-06-09T11-00-00-bbbb.jsonl', [tokenCountLine(null)]);

    const sourceMtime = fs.statSync(sourceFile).mtimeMs;
    const quota = await getCodexLocalQuota({ env, now: sourceMtime + 6 * 60 * 1000 });
    expect(quota?.stale).toBe(true);
    expect(quota?.staleAsOf).toBe(new Date(sourceMtime).toISOString());
    expect(quota?.tier).toBe('plus');
  });

  it('is fresh (no staleAsOf) when the source file is recent', async () => {
    const { env, sessions } = freshHome('fresh');
    const file = writeRollout(sessions, 'rollout-2026-06-09T10-00-00-aaaa.jsonl', [
      tokenCountLine({
        primary: { used_percent: 10, window_minutes: 300, resets_at: 1781033803 },
        secondary: { used_percent: 5, window_minutes: 10080, resets_at: 1781192122 },
        plan_type: 'plus',
      }),
    ]);
    const mtime = fs.statSync(file).mtimeMs;
    const quota = await getCodexLocalQuota({ env, now: mtime + 60 * 1000 });
    expect(quota?.stale).toBe(false);
    expect(quota?.staleAsOf).toBeNull();
  });

  it('tails large rollout files without losing quota near the end', async () => {
    const { env, sessions } = freshHome('large-tail');
    const day = path.join(sessions, '2026', '06', '09');
    fs.mkdirSync(day, { recursive: true });
    const file = path.join(day, 'rollout-2026-06-09T10-00-00-aaaa.jsonl');

    const fd = fs.openSync(file, 'w');
    try {
      fs.writeSync(fd, `${'x'.repeat(2 * 1024 * 1024)}\n`);
      fs.writeSync(
        fd,
        `${tokenCountLine({
          primary: { used_percent: 20, window_minutes: 300, resets_at: 1781033803 },
          secondary: { used_percent: 35, window_minutes: 10080, resets_at: 1781192122 },
          plan_type: 'pro',
        })}\n`
      );
    } finally {
      fs.closeSync(fd);
    }

    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    expect(quota).not.toBeNull();
    expect(quota?.quotaPercentage).toBe(65);
    expect(quota?.tier).toBe('pro');
  });

  it('returns null when there are no rollout files at all', async () => {
    const { env } = freshHome('empty');
    const quota = await getCodexLocalQuota({ env, now: Date.now() });
    expect(quota).toBeNull();
  });
});

describe('getCodexLocalQuota walk pruning', () => {
  const QUOTA = {
    primary: { used_percent: 20, window_minutes: 300, resets_at: 1781033803 },
    secondary: { used_percent: 30, window_minutes: 10080, resets_at: 1781192122 },
    plan_type: 'pro',
  };

  function recordingReaddir(visited: string[]) {
    return (dir: string): fs.Dirent[] => {
      visited.push(dir);
      return fs.readdirSync(dir, { withFileTypes: true });
    };
  }

  it('finds the newest sessions in a YYYY/MM/DD tree without walking old partitions', async () => {
    const { env, sessions } = freshHome('prune-nested');
    // 40 days x 3 files; only the newest file carries quota.
    for (let i = 0; i < 40; i++) {
      const day = new Date(Date.UTC(2026, 4, 1 + i));
      const y = String(day.getUTCFullYear());
      const m = String(day.getUTCMonth() + 1).padStart(2, '0');
      const d = String(day.getUTCDate()).padStart(2, '0');
      const dir = path.join(sessions, y, m, d);
      fs.mkdirSync(dir, { recursive: true });
      for (const hh of ['10', '11', '12']) {
        const newest = i === 39 && hh === '12';
        const file = path.join(dir, `rollout-${y}-${m}-${d}T${hh}-00-00-aaaa.jsonl`);
        fs.writeFileSync(file, tokenCountLine(newest ? QUOTA : null) + '\n');
      }
    }

    const visited: string[] = [];
    const quota = await getCodexLocalQuota({
      env,
      readdirImpl: recordingReaddir(visited),
      now: Date.now(),
    });
    expect(quota?.tier).toBe('pro');
    expect(quota?.quotaPercentage).toBe(70);
    // root + 2026 + 06 + the 7 newest day dirs (3x7 = 21 >= 20 files); a full
    // walk would need 44 readdirs (root + year + 2 months + 40 days).
    expect(visited).toHaveLength(10);
    expect(visited).not.toContain(path.join(sessions, '2026', '05'));
    expect(visited).not.toContain(path.join(sessions, '2026', '06', '02'));
  });

  it('prunes combined YYYY-MM-DD partitions the same way', async () => {
    const { env, sessions } = freshHome('prune-combined');
    for (let i = 1; i <= 25; i++) {
      const d = String(i).padStart(2, '0');
      const dir = path.join(sessions, `2026-06-${d}`);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(
        path.join(dir, `rollout-2026-06-${d}T12-00-00-bbbb.jsonl`),
        tokenCountLine(i === 25 ? QUOTA : null) + '\n'
      );
    }

    const visited: string[] = [];
    const quota = await getCodexLocalQuota({
      env,
      readdirImpl: recordingReaddir(visited),
      now: Date.now(),
    });
    expect(quota?.tier).toBe('pro');
    // Root + the 20 newest day dirs; the oldest 5 partitions are never opened.
    expect(visited).toHaveLength(21);
    expect(visited).toContain(path.join(sessions, '2026-06-25'));
    expect(visited).not.toContain(path.join(sessions, '2026-06-01'));
  });

  it('falls back to the full walk when a non-date dir shares the tree', async () => {
    const { env, sessions } = freshHome('prune-mixed');
    for (let i = 1; i <= 9; i++) {
      const d = String(i).padStart(2, '0');
      const dir = path.join(sessions, '2026', '06', d);
      fs.mkdirSync(dir, { recursive: true });
      for (const hh of ['10', '11', '12']) {
        fs.writeFileSync(
          path.join(dir, `rollout-2026-06-${d}T${hh}-00-00-aaaa.jsonl`),
          tokenCountLine(null) + '\n'
        );
      }
    }
    const archive = path.join(sessions, 'archive');
    fs.mkdirSync(archive, { recursive: true });
    fs.writeFileSync(
      path.join(archive, 'rollout-2026-06-09T23-00-00-cccc.jsonl'),
      tokenCountLine({ ...QUOTA, plan_type: 'plus' }) + '\n'
    );

    const visited: string[] = [];
    const quota = await getCodexLocalQuota({
      env,
      readdirImpl: recordingReaddir(visited),
      now: Date.now(),
    });
    expect(quota?.tier).toBe('plus');
    // Full walk: root + 2026 + 06 + 9 day dirs + archive = 13; nothing pruned.
    expect(visited).toHaveLength(13);
    expect(visited).toContain(archive);
    expect(visited).toContain(path.join(sessions, '2026', '06', '01'));
  });

  it('stops at the directory cap on a huge non-date layout, newest-first so far', async () => {
    const { env, sessions } = freshHome('walk-cap');
    const dirEntry = (name: string): fs.Dirent =>
      ({ name, isDirectory: () => true, isFile: () => false }) as unknown as fs.Dirent;
    const fileEntry = (name: string): fs.Dirent =>
      ({ name, isDirectory: () => false, isFile: () => true }) as unknown as fs.Dirent;
    const rootEntries = Array.from({ length: 60_000 }, (_, i) =>
      dirEntry(`z${String(i).padStart(5, '0')}`)
    );
    let readdirCalls = 0;
    const readdirImpl = (dir: string): fs.Dirent[] => {
      readdirCalls++;
      if (dir === sessions) return rootEntries;
      const base = path.basename(dir);
      return [fileEntry(`rollout-2026-01-01T00-00-00-${base}.jsonl`)];
    };

    const tailed: string[] = [];
    const quota = await getCodexLocalQuota({
      env,
      existsSyncImpl: () => true,
      readdirImpl,
      statMtimeMsImpl: () => Date.now(),
      tailLinesImpl: async (file) => {
        tailed.push(file);
        return [tokenCountLine({ ...QUOTA, plan_type: 'capped' })];
      },
      now: Date.now(),
    });
    // Cap stops after 50k dirs (root + 49_999 children) without throwing, and
    // the newest file among those visited still supplies the quota.
    expect(readdirCalls).toBe(50_000);
    expect(quota?.tier).toBe('capped');
    expect(tailed[0]).toBe(
      path.join(sessions, 'z49998', 'rollout-2026-01-01T00-00-00-z49998.jsonl')
    );
  });
});
