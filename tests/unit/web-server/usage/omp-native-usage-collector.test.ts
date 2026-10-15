import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  isOmpSessionFilename,
  ompSessionIdForFile,
  parseOmpUsageLine,
  resolveOmpSessionRoots,
  OMP_ROOTS_RULE_VERSION,
  OMP_SCAN_MAX_ROOTS,
  OMP_SCAN_CACHE_TTL_MS,
} from '../../../../src/web-server/usage/omp-native-usage-collector';
import { collectAccountActivity } from '../../../../src/web-server/usage/account-activity-collector';

function assistant(overrides: Record<string, unknown> = {}) {
  const { message: messageOverrides, ...rest } = overrides;
  return {
    id: 'msg-1',
    parentId: 's-1',
    timestamp: '2026-10-01T15:05:00Z',
    type: 'message',
    message: {
      role: 'assistant',
      model: 'deepseek-v4.1-flash',
      provider: 'chutes',
      usage: {
        input: 100,
        output: 20,
        cacheRead: 300,
        cacheWrite: 40,
        reasoningTokens: 5,
        totalTokens: 420,
        cost: { input: 0.001, output: 0.002, cacheRead: 0.003, cacheWrite: 0.004, total: 0.01 },
      },
      ...((messageOverrides as Record<string, unknown> | undefined) ?? {}),
    },
    ...rest,
  };
}

describe('omp usage lines', () => {
  it('maps assistant message records, ignoring cumulative counters and snapshots', () => {
    const line = JSON.stringify(
      assistant({
        data: { goal: { tokensUsed: 999999 } },
        message: {
          details: { goal: { tokensUsed: 888888 } },
          contextSnapshot: { promptTokens: 777777 },
        },
      })
    );
    const entry = parseOmpUsageLine(line, '2026-10-01T15-00_abc');
    expect(entry).toMatchObject({
      inputTokens: 100,
      outputTokens: 20,
      cacheReadTokens: 300,
      cacheCreationTokens: 40,
      model: 'deepseek-v4.1-flash',
      sessionId: '2026-10-01T15-00_abc',
      timestamp: '2026-10-01T15:05:00.000Z',
      target: 'omp',
      costUsd: 0.01,
    });
  });

  it('keeps nonzero logged cost but never a logged 0', () => {
    const zero = JSON.stringify(
      assistant({
        message: {
          usage: { input: 10, output: 1, cacheRead: 0, cacheWrite: 0, cost: { total: 0 } },
        },
      })
    );
    expect(parseOmpUsageLine(zero, 's')?.costUsd).toBeUndefined();
    const missing = JSON.stringify(
      assistant({ message: { usage: { input: 10, output: 1, cacheRead: 0, cacheWrite: 0 } } })
    );
    expect(parseOmpUsageLine(missing, 's')?.costUsd).toBeUndefined();
  });

  it('ignores non-usage records, user messages and content lines', () => {
    expect(parseOmpUsageLine(JSON.stringify({ type: 'session', id: 's' }), 's')).toBeNull();
    expect(
      parseOmpUsageLine(JSON.stringify(assistant({ message: { role: 'user' } })), 's')
    ).toBeNull();
    expect(parseOmpUsageLine('just a prompt fragment, not json', 's')).toBeNull();
    expect(
      parseOmpUsageLine(JSON.stringify({ type: 'message', message: { role: 'assistant' } }), 's')
    ).toBeNull();
  });

  it('skips zero-usage pings without cost', () => {
    const line = JSON.stringify(
      assistant({
        message: {
          model: 'union-alpha',
          usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: { total: 0 } },
        },
      })
    );
    expect(parseOmpUsageLine(line, 's')).toBeNull();
  });

  it('derives session ids from file and directory names', () => {
    expect(ompSessionIdForFile('/r/sessions/2026-10-01T15-00_uuid.jsonl')).toBe(
      '2026-10-01T15-00_uuid'
    );
    expect(ompSessionIdForFile('/r/sessions/2026-10-01T15-00_uuid')).toBe('2026-10-01T15-00_uuid');
    expect(ompSessionIdForFile('/r/sessions/2026-10-01T15-00_uuid/__advisor.jsonl')).toBe(
      '2026-10-01T15-00_uuid'
    );
    expect(isOmpSessionFilename('__advisor.jsonl')).toBe(true);
    expect(isOmpSessionFilename('2026-10-01T15-00_uuid.jsonl')).toBe(true);
    expect(isOmpSessionFilename('2026-10-01T15-00_uuid')).toBe(true);
    expect(isOmpSessionFilename('omp.2026-10-01.123.log')).toBe(false);
  });
});

describe('omp session roots', () => {
  let home: string;
  beforeEach(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-omp-roots-'));
  });
  afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

  it('resolves the default, PI_CODING_AGENT_DIR and custom scan roots', async () => {
    fs.mkdirSync(path.join(home, '.omp', 'agent', 'sessions'), { recursive: true });
    fs.mkdirSync(path.join(home, 'custom', 'agent', 'sessions'), { recursive: true });
    const custom = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(custom, { recursive: true });
    fs.writeFileSync(path.join(custom, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    // Skipped directories never contribute roots.
    const skipped = path.join(home, 'PM-Experiments', 'proj', 'node_modules', 'x', 'sessions');
    fs.mkdirSync(skipped, { recursive: true });
    fs.writeFileSync(path.join(skipped, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({
      env: { PI_CODING_AGENT_DIR: path.join(home, 'custom', 'agent') } as NodeJS.ProcessEnv,
      homeDir: home,
    });
    expect(roots).toContain(path.join(home, '.omp', 'agent', 'sessions'));
    expect(roots).toContain(path.join(home, 'custom', 'agent', 'sessions'));
    expect(roots).toContain(custom);
    expect(roots).not.toContain(skipped);
  });

  it('never follows a symlink to a folder outside the home under PM-Experiments', async () => {
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-omp-outside-'));
    try {
      const remote = path.join(outside, 'proj', 'sessions');
      fs.mkdirSync(remote, { recursive: true });
      fs.writeFileSync(path.join(remote, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
      fs.mkdirSync(path.join(home, 'PM-Experiments'), { recursive: true });
      fs.symlinkSync(outside, path.join(home, 'PM-Experiments', 'nas-link'));
      const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
      expect(roots.some((root) => root.includes('nas-link') || root.startsWith(outside))).toBe(
        false
      );
    } finally {
      fs.rmSync(outside, { recursive: true, force: true });
    }
  });

  it('treats an OMP-named sessions dir as a root, and empty ones as none', async () => {
    // Presence alone (any `.jsonl`) used to qualify; it accepted synthetic
    // Muse trees, so a `sessions/` dir must now hold an OMP-named file.
    const generic = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(generic, { recursive: true });
    fs.writeFileSync(path.join(generic, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const empty = path.join(home, 'PM-Experiments', 'other', 'sessions');
    fs.mkdirSync(empty, { recursive: true });
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).toContain(generic);
    expect(roots).not.toContain(empty);
  });

  it('honors explicit scan bounds', async () => {
    const custom = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(custom, { recursive: true });
    fs.writeFileSync(path.join(custom, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const starved = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      scanBounds: { maxEntries: 1 },
    });
    expect(starved).not.toContain(custom);
    const found = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(found).toContain(custom);
  });

  it('reports explicit roots cut by the combined root cap', async () => {
    const discovery = { truncated: false };
    const extra = Array.from({ length: OMP_SCAN_MAX_ROOTS }, (_, index) =>
      path.join(home, `extra-${index}`)
    );
    const roots = await resolveOmpSessionRoots({
      homeDir: home,
      env: { OMP_SESSION_DIRS: extra.join(path.delimiter) },
      discovery,
    });
    expect(roots).toHaveLength(OMP_SCAN_MAX_ROOTS);
    expect(discovery.truncated).toBe(true);
  });

  it('discovers and reads depth-nine native sessions, reporting a depth-limited search', async () => {
    const deep = path.join(
      home,
      'PM-Experiments',
      ...Array.from({ length: 8 }, (_, i) => `d${i}`),
      'native_sessions'
    );
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(
      path.join(deep, '2026-10-01T15-00_deep.jsonl'),
      `${JSON.stringify(assistant())}\n`
    );
    const discovery = { truncated: false };
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home, discovery });
    expect(roots).toContain(deep);
    expect(discovery.truncated).toBe(false);
    const data = await collectAccountActivity(
      { kind: 'omp', roots: [deep, path.dirname(deep)] },
      { minDate: Date.parse('2026-09-01'), cacheDir: path.join(home, 'cache') }
    );
    expect(data.eventCount).toBe(1);
    await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      discovery,
      scanBounds: { maxDepth: 2 },
    });
    expect(discovery.truncated).toBe(true);
  });

  it('rebuilds an old rule cache and revalidates cached sandbox roots', async () => {
    const cacheDir = path.join(home, 'cache');
    const deep = path.join(home, 'PM-Experiments', 'exp', 'native_sessions');
    fs.mkdirSync(deep, { recursive: true });
    fs.mkdirSync(cacheDir, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_cache.jsonl'), '{}\n');
    const cache = path.join(cacheDir, 'omp-session-roots-v1.json');
    fs.writeFileSync(
      cache,
      JSON.stringify({
        version: 2,
        ruleVersion: OMP_ROOTS_RULE_VERSION - 1,
        scannedAt: Date.now(),
        truncated: false,
        roots: [],
      })
    );
    expect(await resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir })).toContain(deep);
    expect(JSON.parse(fs.readFileSync(cache, 'utf8')).ruleVersion).toBe(OMP_ROOTS_RULE_VERSION);
    const obsolete = JSON.parse(fs.readFileSync(cache, 'utf8'));
    obsolete.ruleVersion--;
    obsolete.roots = [];
    for (const child of obsolete.children) child.roots = [];
    fs.writeFileSync(cache, JSON.stringify(obsolete));
    expect(await resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir })).toContain(deep);
    fs.writeFileSync(path.join(path.dirname(deep), '.aac-synthetic'), 'fixture\n');
    expect(await resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir })).not.toContain(deep);
  });

  it('reuses completed children until their TTL or directory change signal', async () => {
    const cacheDir = path.join(home, 'cache');
    const child = path.join(home, 'PM-Experiments', 'a');
    const makeRoot = (name: string) => {
      const root = path.join(child, 'container', name);
      fs.mkdirSync(root, { recursive: true });
      fs.writeFileSync(path.join(root, '2026-10-01T15-00_cache.jsonl'), '{}\n');
      return root;
    };
    const first = makeRoot('one-sessions');
    const now = Date.now();
    const stamp = fs.statSync(child).mtimeMs;
    const discovery = { truncated: false };
    const scan = (time: number) =>
      resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir, now: () => time, discovery });
    expect(await scan(now)).toContain(first);
    const second = makeRoot('two-sessions');
    // Nested writes do not change the top-level child's cheap change signal.
    expect(fs.statSync(child).mtimeMs).toBe(stamp);
    expect(await scan(now + 1000)).not.toContain(second);
    expect(await scan(now + OMP_SCAN_CACHE_TTL_MS + 1)).toContain(second);
    expect(discovery.truncated).toBe(false);
    const third = makeRoot('three-sessions');
    fs.utimesSync(child, new Date(now + 60_000), new Date(now + 60_000));
    expect(await scan(now + OMP_SCAN_CACHE_TTL_MS + 1000)).toContain(third);
    expect(discovery.truncated).toBe(false);
    // A completed replacement drops roots that are gone, rather than unioning forever.
    fs.rmSync(second, { recursive: true, force: true });
    fs.utimesSync(child, new Date(now + 120_000), new Date(now + 120_000));
    expect(await scan(now + OMP_SCAN_CACHE_TTL_MS + 2000)).not.toContain(second);
  });

  it('dedupes roots that resolve to the same directory', async () => {
    const roots = await resolveOmpSessionRoots({
      env: { PI_CODING_AGENT_DIR: path.join(home, '.omp', 'agent') } as NodeJS.ProcessEnv,
      homeDir: home,
    });
    expect(
      roots.filter((root) => root === path.join(home, '.omp', 'agent', 'sessions'))
    ).toHaveLength(1);
  });

  it('accepts a custom --session-dir root by content, not the literal name sessions', async () => {
    // Wave-4 workers wrote `<branch>-sessions` dirs (e.g. fw4-t2-sessions) that
    // the old name-only check missed; each holds `<ts>_<uuid>.jsonl` directly.
    const worker = path.join(home, 'PM-Experiments', 'proj', 'worktrees', 'omp', 'fw4-t2-sessions');
    fs.mkdirSync(worker, { recursive: true });
    fs.writeFileSync(path.join(worker, '2026-10-05T10-07-46-206Z_01a10b88_uuid.jsonl'), '{}\n');
    // A subagent subdir is reached by the collector's own recursive read; an
    // accepted root is never descended, so it must not become a separate root.
    const sub = path.join(worker, '2026-10-05T10-07-46-206Z_01a10b88_uuid');
    fs.mkdirSync(sub, { recursive: true });
    fs.writeFileSync(path.join(sub, '__advisor.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).toContain(worker);
    expect(roots).not.toContain(sub);
  });

  it('does not accept a non-sessions dir that holds only non-session jsonl', async () => {
    const src = path.join(home, 'PM-Experiments', 'proj', 'src');
    fs.mkdirSync(src, { recursive: true });
    fs.writeFileSync(path.join(src, 'notes.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).not.toContain(src);
  });

  it('enumerates per-profile OMP session roots under ~/.omp/profiles', async () => {
    const profile = path.join(home, '.omp', 'profiles', 'pm-probe', 'agent', 'sessions');
    fs.mkdirSync(profile, { recursive: true });
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).toContain(profile);
  });

  it('examines a sessions dir one level past the depth cap', async () => {
    // Experiment runners nest per-job session dirs one level deeper than the
    // scan's depth cap (`runs/<run>/jobs/<job>/sessions` is depth 7); the walk examines
    // session-container children of a max-depth dir inline, without
    // descending further. Use a tiny maxDepth so the fixture stays small.
    const deep = path.join(
      home,
      'PM-Experiments',
      'exp',
      'runs',
      'r1',
      'stage',
      'jobs',
      'j1',
      'sessions'
    );
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      scanBounds: { maxDepth: 6 },
    });
    expect(roots).toContain(deep);
  });

  it('excludes a marked sandbox tree while a sibling worker sessions root still counts', async () => {
    // Sandbox generators write `.aac-synthetic` at their data-tree root; the
    // scan skips the whole subtree so fixtures never count as real usage.
    const data = path.join(home, 'PM-Experiments', 'worktrees', 'omp', 'fw4-t9x-run', 'data');
    const sandbox = path.join(data, 'omp', 'sessions');
    fs.mkdirSync(sandbox, { recursive: true });
    fs.writeFileSync(path.join(sandbox, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    fs.writeFileSync(path.join(data, '.aac-synthetic'), 'synthetic\n');
    const worker = path.join(home, 'PM-Experiments', 'worktrees', 'omp', 'fw4-n4o-sessions');
    fs.mkdirSync(worker, { recursive: true });
    fs.writeFileSync(path.join(worker, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).not.toContain(sandbox);
    expect(roots).toContain(worker);
  });

  it('excludes a marked sessions dir one level past the depth cap', async () => {
    const deep = path.join(
      home,
      'PM-Experiments',
      'exp',
      'runs',
      'r1',
      'stage',
      'jobs',
      'j1',
      'sessions'
    );
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    fs.writeFileSync(path.join(deep, '.aac-synthetic'), 'synthetic\n');
    const roots = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      scanBounds: { maxDepth: 6 },
    });
    expect(roots).not.toContain(deep);
  });

  it('skips generator manifests, non-OMP sessions dirs and fixture dirs', async () => {
    // T5-recipe sandboxes predate the marker: their data/MANIFEST.json with a
    // `trees` key marks the tree instead.
    const data = path.join(home, 'PM-Experiments', 'worktrees', 'omp', 'fw4-t9x-run', 'data');
    const legacy = path.join(data, 'omp', 'sessions');
    fs.mkdirSync(legacy, { recursive: true });
    fs.writeFileSync(path.join(legacy, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    fs.writeFileSync(path.join(data, 'MANIFEST.json'), JSON.stringify({ trees: { omp: {} } }));
    // Synthetic Muse trees (`<uuid>/session.jsonl`) are not OMP-named
    // (kept outside the manifest tree so this isolates the name rule).
    const museRoot = path.join(home, 'PM-Experiments', 'other', 'sessions');
    const muse = path.join(museRoot, 'some-uuid');
    fs.mkdirSync(muse, { recursive: true });
    fs.writeFileSync(path.join(muse, 'session.jsonl'), '{}\n');
    // Fixture/test/build dirs are never descended.
    const hidden = path.join(home, 'PM-Experiments', 'exp', 'tests', 'sessions');
    fs.mkdirSync(hidden, { recursive: true });
    fs.writeFileSync(path.join(hidden, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).not.toContain(legacy);
    expect(roots).not.toContain(museRoot);
    expect(roots).not.toContain(hidden);
  });

  it('does not examine a non-sessions dir past the depth cap', async () => {
    // The one-level-past-the-cap exception is only for session containers:
    // an `evidence/` dir holding a session file one level too deep stays out
    // of reach (explicit extra usage-log sources cover those).
    const deep = path.join(
      home,
      'PM-Experiments',
      'exp',
      'runs',
      'r1',
      'stage',
      'jobs',
      'j1',
      'evidence'
    );
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      scanBounds: { maxDepth: 6 },
    });
    expect(roots).not.toContain(deep);
  });

  it('resumes large children across collections and becomes complete without advancing a retry TTL', async () => {
    const cacheDir = path.join(home, 'cache');
    const expected: string[] = [];
    for (const child of ['a', 'b']) {
      const base = path.join(home, 'PM-Experiments', child);
      for (let index = 0; index < 5; index++)
        fs.mkdirSync(path.join(base, `padding-${index}`), { recursive: true });
      const root = path.join(base, 'sessions');
      fs.mkdirSync(root, { recursive: true });
      fs.writeFileSync(path.join(root, `2026-10-01T15-00_${child}.jsonl`), '{}\n');
      expected.push(root);
    }
    const discovery = { truncated: false };
    let roots: string[] = [];
    let passes = 0;
    const now = Date.now();
    do {
      roots = await resolveOmpSessionRoots({
        env: {},
        homeDir: home,
        cacheDir,
        now: () => now,
        discovery,
        scanBounds: { maxDirs: 2 },
      });
      passes++;
      expect(passes).toBeLessThan(20);
    } while (discovery.truncated && passes < 20);
    expect(passes).toBeGreaterThan(2);
    expect(discovery.truncated).toBe(false);
    expect(roots.filter((root) => root.includes('PM-Experiments')).sort()).toEqual(expected.sort());
    const cache = JSON.parse(
      fs.readFileSync(path.join(cacheDir, 'omp-session-roots-v1.json'), 'utf8')
    );
    expect(
      cache.children.every((child: { pending: unknown[] }) => child.pending.length === 0)
    ).toBe(true);
    expect(cache.truncated).toBe(false);
  });

  it('walks new children before resuming a large existing child, then rotates unfinished children', async () => {
    const cacheDir = path.join(home, 'cache');
    for (const name of ['a', 'b']) {
      const experiment = path.join(home, 'PM-Experiments', name);
      for (let index = 0; index < 6; index++)
        fs.mkdirSync(path.join(experiment, `padding-${index}`), { recursive: true });
    }
    const discovery = { truncated: false };
    const scan = () =>
      resolveOmpSessionRoots({
        env: {},
        homeDir: home,
        cacheDir,
        discovery,
        scanBounds: { maxDirs: 1 },
      });
    await scan();
    expect(discovery.truncated).toBe(true);
    const newRoot = path.join(home, 'PM-Experiments', 'z-new-sessions');
    fs.mkdirSync(newRoot);
    fs.writeFileSync(path.join(newRoot, '2026-10-01T15-00_new.jsonl'), '{}\n');
    expect(await scan()).toContain(newRoot);
    expect(discovery.truncated).toBe(true);
    const before = JSON.parse(
      fs.readFileSync(path.join(cacheDir, 'omp-session-roots-v1.json'), 'utf8')
    );
    const aBefore = before.children.find((child: { name: string }) => child.name === 'a');
    await scan();
    const rotated = JSON.parse(
      fs.readFileSync(path.join(cacheDir, 'omp-session-roots-v1.json'), 'utf8')
    );
    expect(rotated.children.find((child: { name: string }) => child.name === 'b').lastPass).toBe(
      rotated.pass
    );
    expect(rotated.children.find((child: { name: string }) => child.name === 'a').lastPass).toBe(
      aBefore.lastPass
    );
    await scan();
    const after = JSON.parse(
      fs.readFileSync(path.join(cacheDir, 'omp-session-roots-v1.json'), 'utf8')
    );
    expect(
      after.children.find((child: { name: string }) => child.name === 'a').pending.length
    ).toBe(aBefore.pending.length - 1);
  });

  it('resumes an entry-budget cut without restarting the child', async () => {
    const cacheDir = path.join(home, 'cache');
    const experiment = path.join(home, 'PM-Experiments', 'a');
    const root = path.join(experiment, 'b', 'sessions');
    fs.mkdirSync(root, { recursive: true });
    fs.mkdirSync(path.join(experiment, 'z'));
    fs.mkdirSync(path.join(experiment, 'b', 'z'));
    fs.writeFileSync(path.join(root, '2026-10-01T15-00_entries.jsonl'), '{}\n');
    const discovery = { truncated: false };
    const scan = () =>
      resolveOmpSessionRoots({
        env: {},
        homeDir: home,
        cacheDir,
        discovery,
        scanBounds: { maxEntries: 3 },
      });
    expect(await scan()).not.toContain(root);
    expect(discovery.truncated).toBe(true);
    let roots: string[] = [];
    for (let pass = 0; pass < 5 && discovery.truncated; pass++) roots = await scan();
    expect(roots).toContain(root);
    expect(discovery.truncated).toBe(false);
  });

  it('retains the discovery root cap and reports overflow after the walk finishes', async () => {
    const base = path.join(home, 'PM-Experiments');
    for (let index = 0; index <= OMP_SCAN_MAX_ROOTS; index++) {
      const root = path.join(base, `root-${index}-sessions`);
      fs.mkdirSync(root, { recursive: true });
      fs.writeFileSync(path.join(root, '2026-10-01T15-00_cap.jsonl'), '{}\n');
    }
    const cacheDir = path.join(home, 'cache');
    const discovery = { truncated: false };
    const roots = await resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir, discovery });
    expect(roots).toHaveLength(OMP_SCAN_MAX_ROOTS);
    expect(discovery.truncated).toBe(true);
    const cache = JSON.parse(
      fs.readFileSync(path.join(cacheDir, 'omp-session-roots-v1.json'), 'utf8')
    );
    expect(cache.roots).toHaveLength(OMP_SCAN_MAX_ROOTS);
    expect(
      cache.children.every((child: { pending: unknown[] }) => child.pending.length === 0)
    ).toBe(true);
  });

  it('revalidates resumed ancestors when a sandbox marker appears between passes', async () => {
    const cacheDir = path.join(home, 'cache');
    const experiment = path.join(home, 'PM-Experiments', 'a');
    const roots = ['one-sessions', 'two-sessions'].map((name) => path.join(experiment, name));
    for (const root of roots) {
      fs.mkdirSync(root, { recursive: true });
      fs.writeFileSync(path.join(root, '2026-10-01T15-00_sandbox.jsonl'), '{}\n');
    }
    const discovery = { truncated: false };
    const scan = () =>
      resolveOmpSessionRoots({
        env: {},
        homeDir: home,
        cacheDir,
        discovery,
        scanBounds: { maxDirs: 2 },
      });
    expect(await scan()).toContain(roots[0]);
    expect(discovery.truncated).toBe(true);
    fs.writeFileSync(path.join(experiment, '.aac-synthetic'), 'fixture\n');
    const next = await scan();
    expect(next).not.toContain(roots[0]);
    expect(next).not.toContain(roots[1]);
    expect(discovery.truncated).toBe(false);
  });
});
