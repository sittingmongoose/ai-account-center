import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  isOmpSessionFilename,
  ompSessionIdForFile,
  parseOmpUsageLine,
  resolveOmpSessionRoots,
} from '../../../../src/web-server/usage/omp-native-usage-collector';

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

  it('caches the marker scan for six hours', async () => {
    const cacheDir = path.join(home, 'cache');
    const first = path.join(home, 'PM-Experiments', 'a', 'sessions');
    fs.mkdirSync(first, { recursive: true });
    fs.writeFileSync(path.join(first, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const now = Date.now();
    const scanned = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now,
    });
    expect(scanned).toContain(first);
    const second = path.join(home, 'PM-Experiments', 'b', 'sessions');
    fs.mkdirSync(second, { recursive: true });
    fs.writeFileSync(path.join(second, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const cached = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now + 1000,
    });
    expect(cached).not.toContain(second);
    const rescanned = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now + 6 * 3_600_000 + 1,
    });
    expect(rescanned).toContain(second);
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

  it('rescans a truncated marker scan on a short TTL and unions roots', async () => {
    const cacheDir = path.join(home, 'cache');
    const dirA = path.join(home, 'PM-Experiments', 'a-sessions');
    fs.mkdirSync(dirA, { recursive: true });
    fs.writeFileSync(path.join(dirA, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const cacheFile = path.join(cacheDir, 'omp-session-roots-v1.json');
    const now = Date.now();
    // Starve the first walk so it truncates before reaching dirA.
    await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now,
      scanBounds: { maxDirs: 1 },
    });
    const first = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
    expect(first.truncated).toBe(true);
    // A truncated cache is not frozen for six hours: past its short TTL the walk
    // reruns, converges on the small tree, and unions dirA into the cache.
    const later = await resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now + 31 * 60_000,
    });
    expect(later).toContain(dirA);
    const second = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
    expect(second.truncated).toBe(false);
    expect(second.roots).toContain(dirA);
  });
});
