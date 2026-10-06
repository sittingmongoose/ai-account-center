import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { Database } from 'bun:sqlite';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  readExperimentRoots,
  runExperimentRootsSlice,
  EXPERIMENT_ROOTS_TTL_MS,
} from '../../../../src/web-server/usage/experiment-usage-roots';
import {
  classifyUsageLogText,
  experimentRootFor,
} from '../../../../src/web-server/usage/experiment-usage-signatures';

const transcript = {
  type: 'assistant',
  sessionId: 's-1',
  version: '2.1.0',
  cwd: '/work',
  uuid: 'u-1',
  requestId: 'req-1',
  timestamp: '2026-10-01T15:00:00Z',
  message: { id: 'msg-1', model: 'claude-opus-5-5', usage: { input_tokens: 1, output_tokens: 2 } },
};
const streamInit = {
  type: 'system',
  subtype: 'init',
  session_id: 's-2',
  claude_code_version: '2.1.0',
  cwd: '/work',
};
const streamAssistant = {
  type: 'assistant',
  session_id: 's-2',
  uuid: 'u-2',
  request_id: 'req-2',
  timestamp: '2026-10-01T15:00:00Z',
  message: { id: 'msg-2', model: 'claude-opus-5-5', usage: { input_tokens: 1, output_tokens: 2 } },
};
// The AAC generators' synthetic records carry only the usage fields.
const syntheticClaude = {
  type: 'assistant',
  sessionId: 's-3',
  uuid: 'u-3',
  timestamp: '2026-10-01T15:00:00Z',
  message: { id: 'msg-3', model: 'claude-opus-5-5', usage: { input_tokens: 1, output_tokens: 2 } },
};
const codexMeta = {
  timestamp: '2026-10-01T15:00:00Z',
  type: 'session_meta',
  payload: { id: 'c-1', cwd: '/work', originator: 'codex_cli_rs', cli_version: '0.99.0' },
};
const syntheticCodexMeta = {
  type: 'session_meta',
  payload: { id: 'c-2', timestamp: '2026-10-01T15:00:00Z', model_provider: 'openai' },
};
const museEvent = {
  record_type: 'event',
  recorded_at: 1790695015000,
  id: 'rec-1',
  stream: 'session',
  payload: { event: { kind: 'started' } },
};
const syntheticMuse = {
  schema_version: 1,
  recorded_at: 1790695015000,
  payload: { event: { kind: 'model_completed', usage: { input_tokens: 1 } } },
};

const lines = (...records: unknown[]) => records.map((r) => JSON.stringify(r)).join('\n') + '\n';

describe('experiment usage log signatures', () => {
  it('recognizes each tool by its own record fields', () => {
    expect(classifyUsageLogText(lines(transcript), 'a.jsonl')).toBe('claude');
    expect(classifyUsageLogText(lines(streamInit, streamAssistant), 'stream.jsonl')).toBe('claude');
    expect(classifyUsageLogText(lines(streamAssistant), 'x-stream.jsonl')).toBe('claude');
    expect(classifyUsageLogText(lines(codexMeta), 'rollout-2026-10-01T15-00-00-c1.jsonl')).toBe(
      'codex'
    );
    expect(classifyUsageLogText(lines(museEvent), 'session.jsonl')).toBe('muse');
  });

  it('rejects the synthetic fixture shapes the AAC generators write', () => {
    expect(classifyUsageLogText(lines(syntheticClaude), 'a.jsonl')).toBeNull();
    expect(classifyUsageLogText(lines(syntheticCodexMeta), 'rollout-x.jsonl')).toBeNull();
    expect(classifyUsageLogText(lines(syntheticMuse), 'session.jsonl')).toBeNull();
    // Right record, wrong file: a Muse envelope outside session.jsonl, a rollout record elsewhere.
    expect(classifyUsageLogText(lines(museEvent), 'events.jsonl')).toBeNull();
    expect(classifyUsageLogText(lines(codexMeta), 'events.jsonl')).toBeNull();
  });

  it('roots Codex and Muse logs above their date folders', () => {
    expect(experimentRootFor('codex', '/x/home/sessions/2026/10/01/rollout-a.jsonl')).toBe(
      '/x/home/sessions'
    );
    expect(experimentRootFor('muse', '/x/data/sessions/2026/10/01/abc/session.jsonl')).toBe(
      '/x/data/sessions'
    );
    expect(experimentRootFor('codex', '/x/loose/rollout-a.jsonl')).toBe('/x/loose');
    expect(experimentRootFor('muse', '/x/loose/abc/session.jsonl')).toBe('/x/loose');
    expect(experimentRootFor('claude', '/x/jobs/J1/sessions/s.jsonl')).toBe('/x/jobs/J1/sessions');
    expect(experimentRootFor('zcode', '/x/zc/cli/db/db.sqlite')).toBe('/x/zc/cli/db/db.sqlite');
  });
});

describe('experiment usage root discovery', () => {
  let home: string;
  let base: string;
  let cacheDir: string;
  const write = (relative: string, text: string) => {
    const file = path.join(base, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    return file;
  };
  const zcodeDb = (relative: string, rows = 1) => {
    const file = path.join(base, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const db = new Database(file);
    db.run(
      'CREATE TABLE model_usage (id TEXT PRIMARY KEY, session_id TEXT, model_id TEXT, provider_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER)'
    );
    for (let index = 0; index < rows; index++)
      db.run('INSERT INTO model_usage VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)', [
        `r${index}`,
        's',
        'glm-5',
        'zai',
        1790000000000 + index,
        10,
        2,
        0,
        0,
      ]);
    db.close();
    return file;
  };
  const scanAll = async (options: Record<string, number> = {}) => {
    let slices = 0;
    while (await runExperimentRootsSlice({ homeDir: home, cacheDir, ...options })) slices++;
    return slices + 1;
  };
  beforeEach(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-exp-roots-'));
    base = path.join(home, 'PM-Experiments');
    cacheDir = path.join(home, 'cache');
    fs.mkdirSync(base, { recursive: true });
  });
  afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

  it('finds every tool by content in private tool homes, at any folder name', async () => {
    write('exp-a/runs/r1/jobs/J1/sessions/abc.jsonl', lines(transcript));
    write(
      'exp-a/runs/r1/jobs/J1/raw-claude/claude-stream.jsonl',
      lines(streamInit, streamAssistant)
    );
    write(
      'exp-b/attempt/private/whatever-home/sessions/2026/10/01/rollout-x.jsonl',
      lines(codexMeta)
    );
    write(
      'exp-b/attempt/private/whatever-home/sessions/2026/10/02/rollout-y.jsonl',
      lines(codexMeta)
    );
    write('exp-c/attempt/private/m-data/sessions/2026/10/01/s1/session.jsonl', lines(museEvent));
    zcodeDb('exp-d/attempt/private/zc/cli/db/db.sqlite');
    write('exp-e/notes/plain.jsonl', lines({ hello: 'world' }));
    // Another tool's table named model_usage, with other columns, is not zcode.
    const other = path.join(base, 'exp-f/profile/agent.db');
    fs.mkdirSync(path.dirname(other), { recursive: true });
    const foreign = new Database(other);
    foreign.run('CREATE TABLE model_usage (id INTEGER PRIMARY KEY, model TEXT, tokens INTEGER)');
    foreign.close();
    await scanAll();
    const view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.scanning).toBe(false);
    expect(view.roots.claude.sort()).toEqual([
      path.join(base, 'exp-a/runs/r1/jobs/J1/raw-claude'),
      path.join(base, 'exp-a/runs/r1/jobs/J1/sessions'),
    ]);
    expect(view.roots.codex).toEqual([
      path.join(base, 'exp-b/attempt/private/whatever-home/sessions'),
    ]);
    expect(view.roots.muse).toEqual([path.join(base, 'exp-c/attempt/private/m-data/sessions')]);
    expect(view.roots.zcode).toEqual([
      path.join(base, 'exp-d/attempt/private/zc/cli/db/db.sqlite'),
    ]);
  });

  it('never returns a synthetic sandbox, its fake homes, or fixture and build folders', async () => {
    // A generator sandbox: the marked data tree plus a fake home beside it.
    write('aac/run1/data/.aac-synthetic', 'synthetic\n');
    write('aac/run1/data/claude/projects/p/a.jsonl', lines(transcript));
    write('aac/run1/home-n9/.claude/projects/p/a.jsonl', lines(transcript));
    write('aac/run1/fake-mac/.codex/sessions/2026/10/01/rollout-a.jsonl', lines(codexMeta));
    // A legacy sandbox (generator MANIFEST with trees) and a SANDBOX.md tree.
    write('aac/run2/data/MANIFEST.json', JSON.stringify({ trees: { claude: {} } }));
    write('aac/run2/home/.claude/projects/p/a.jsonl', lines(transcript));
    write('aac/run3/SANDBOX.md', '# sandbox\n');
    write('aac/run3/x/a.jsonl', lines(transcript));
    for (const skip of ['tests', 'fixtures', 'node_modules', 'dist', 'target'])
      write(`repo/${skip}/claude/a.jsonl`, lines(transcript));
    // A real experiment beside the SANDBOX.md tree still counts.
    write('aac/run3-real/sessions/a.jsonl', lines(transcript));
    await scanAll();
    const view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.roots.claude).toEqual([path.join(base, 'aac/run3-real/sessions')]);
    expect(view.roots.codex).toEqual([]);
  });

  it('reads a byte-identical zcode copy once', async () => {
    const original = zcodeDb('exp/a/private/zc/cli/db/db.sqlite', 3);
    const copy = path.join(base, 'exp/a/native/zc-copy/cli/db/db.sqlite');
    fs.mkdirSync(path.dirname(copy), { recursive: true });
    fs.copyFileSync(original, copy);
    // A read-only open can leave an empty WAL beside one copy: still the same content.
    fs.writeFileSync(`${original}-wal`, '');
    zcodeDb('exp/b/private/zc/cli/db/db.sqlite', 5);
    await scanAll();
    const view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.roots.zcode).toEqual([
      path.join(base, 'exp/a/native/zc-copy/cli/db/db.sqlite'),
      path.join(base, 'exp/b/private/zc/cli/db/db.sqlite'),
    ]);
  });

  it('resumes across bounded slices and reaches the same roots as one pass', async () => {
    for (let index = 0; index < 30; index++)
      write(`exp/run-${index}/deep/er/sessions/a.jsonl`, lines(transcript));
    const slices = await scanAll({ sliceMaxDirs: 7 });
    expect(slices).toBeGreaterThan(5);
    const sliced = readExperimentRoots(cacheDir, { homeDir: home }).roots.claude;
    fs.rmSync(cacheDir, { recursive: true, force: true });
    expect(await scanAll()).toBe(1);
    expect(readExperimentRoots(cacheDir, { homeDir: home }).roots.claude).toEqual(sliced);
    expect(sliced).toHaveLength(30);
  });

  it('publishes roots found so far while a round runs', async () => {
    for (let index = 0; index < 10; index++)
      write(`exp/run-${index}/sessions/a.jsonl`, lines(transcript));
    expect(await runExperimentRootsSlice({ homeDir: home, cacheDir, sliceMaxDirs: 6 })).toBe(true);
    const partial = readExperimentRoots(cacheDir, { homeDir: home });
    expect(partial.scanning).toBe(true);
    expect(partial.roots.claude.length).toBeGreaterThan(0);
    expect(partial.roots.claude.length).toBeLessThan(10);
  });

  it('holds its caps: roots per kind, depth, stack and directories', async () => {
    for (let index = 0; index < 5; index++)
      write(`exp/run-${index}/sessions/a.jsonl`, lines(transcript));
    write('deep/1/2/3/4/5/6/7/8/9/10/11/12/13/14/15/a.jsonl', lines(transcript));
    await scanAll({ maxRoots: 3 });
    let view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.roots.claude).toHaveLength(3);
    expect(view.capped).toEqual(['claude']);
    expect(view.roots.claude.some((root) => root.includes('/15'))).toBe(false);
    fs.rmSync(cacheDir, { recursive: true, force: true });
    await scanAll({ maxStack: 2 });
    view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.truncated).toBe(true);
    fs.rmSync(cacheDir, { recursive: true, force: true });
    await scanAll({ maxDirs: 3 });
    view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.truncated).toBe(true);
    expect(view.scanning).toBe(false);
  });

  it('keeps a completed round for its TTL, then rescans without dropping roots meanwhile', async () => {
    write('exp/one/sessions/a.jsonl', lines(transcript));
    let clock = Date.parse('2026-10-06T00:00:00Z');
    const now = () => clock;
    await runExperimentRootsSlice({ homeDir: home, cacheDir, now });
    write('exp/two/sessions/a.jsonl', lines(transcript));
    clock += 60_000;
    expect(await runExperimentRootsSlice({ homeDir: home, cacheDir, now })).toBe(false);
    expect(readExperimentRoots(cacheDir, { homeDir: home }).roots.claude).toHaveLength(1);
    clock += EXPERIMENT_ROOTS_TTL_MS;
    expect(await runExperimentRootsSlice({ homeDir: home, cacheDir, now, sliceMaxDirs: 1 })).toBe(
      true
    );
    expect(readExperimentRoots(cacheDir, { homeDir: home }).roots.claude).toHaveLength(1);
    while (await runExperimentRootsSlice({ homeDir: home, cacheDir, now })) clock += 1;
    expect(readExperimentRoots(cacheDir, { homeDir: home }).roots.claude).toHaveLength(2);
  });

  it('completes an empty round when there is no experiments folder', async () => {
    fs.rmSync(base, { recursive: true, force: true });
    expect(await runExperimentRootsSlice({ homeDir: home, cacheDir })).toBe(false);
    const view = readExperimentRoots(cacheDir, { homeDir: home });
    expect(view.scanning).toBe(false);
    expect(view.roots.claude).toEqual([]);
  });
});
