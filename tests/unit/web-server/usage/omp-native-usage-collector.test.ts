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

  it('resolves the default, PI_CODING_AGENT_DIR and custom scan roots', () => {
    fs.mkdirSync(path.join(home, '.omp', 'agent', 'sessions'), { recursive: true });
    fs.mkdirSync(path.join(home, 'custom', 'agent', 'sessions'), { recursive: true });
    const custom = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(custom, { recursive: true });
    fs.writeFileSync(path.join(custom, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    // Skipped directories never contribute roots.
    const skipped = path.join(home, 'PM-Experiments', 'proj', 'node_modules', 'x', 'sessions');
    fs.mkdirSync(skipped, { recursive: true });
    fs.writeFileSync(path.join(skipped, '2026-10-01T15-00_uuid.jsonl'), '{}\n');
    const roots = resolveOmpSessionRoots({
      env: { PI_CODING_AGENT_DIR: path.join(home, 'custom', 'agent') } as NodeJS.ProcessEnv,
      homeDir: home,
    });
    expect(roots).toContain(path.join(home, '.omp', 'agent', 'sessions'));
    expect(roots).toContain(path.join(home, 'custom', 'agent', 'sessions'));
    expect(roots).toContain(custom);
    expect(roots).not.toContain(skipped);
  });

  it('treats any jsonl-bearing sessions dir as a root, and empty ones as none', () => {
    const generic = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(generic, { recursive: true });
    fs.writeFileSync(path.join(generic, 'rollout-anything.jsonl'), '{}\n');
    const empty = path.join(home, 'PM-Experiments', 'other', 'sessions');
    fs.mkdirSync(empty, { recursive: true });
    const roots = resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(roots).toContain(generic);
    expect(roots).not.toContain(empty);
  });

  it('honors explicit scan bounds', () => {
    const custom = path.join(home, 'PM-Experiments', 'proj', 'sessions');
    fs.mkdirSync(custom, { recursive: true });
    fs.writeFileSync(path.join(custom, 'x.jsonl'), '{}\n');
    const starved = resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      scanBounds: { maxEntries: 1 },
    });
    expect(starved).not.toContain(custom);
    const found = resolveOmpSessionRoots({ env: {}, homeDir: home });
    expect(found).toContain(custom);
  });

  it('caches the marker scan for six hours', () => {
    const cacheDir = path.join(home, 'cache');
    const first = path.join(home, 'PM-Experiments', 'a', 'sessions');
    fs.mkdirSync(first, { recursive: true });
    fs.writeFileSync(path.join(first, 'x.jsonl'), '{}\n');
    const now = Date.now();
    const scanned = resolveOmpSessionRoots({ env: {}, homeDir: home, cacheDir, now: () => now });
    expect(scanned).toContain(first);
    const second = path.join(home, 'PM-Experiments', 'b', 'sessions');
    fs.mkdirSync(second, { recursive: true });
    fs.writeFileSync(path.join(second, 'y.jsonl'), '{}\n');
    const cached = resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now + 1000,
    });
    expect(cached).not.toContain(second);
    const rescanned = resolveOmpSessionRoots({
      env: {},
      homeDir: home,
      cacheDir,
      now: () => now + 6 * 3_600_000 + 1,
    });
    expect(rescanned).toContain(second);
  });

  it('dedupes roots that resolve to the same directory', () => {
    const roots = resolveOmpSessionRoots({
      env: { PI_CODING_AGENT_DIR: path.join(home, '.omp', 'agent') } as NodeJS.ProcessEnv,
      homeDir: home,
    });
    expect(
      roots.filter((root) => root === path.join(home, '.omp', 'agent', 'sessions'))
    ).toHaveLength(1);
  });
});
