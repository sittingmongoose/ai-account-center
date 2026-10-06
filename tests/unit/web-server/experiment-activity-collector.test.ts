import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { Database } from 'bun:sqlite';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
import { experimentActivityRequests } from '../../../src/web-server/services/account-analytics-activity';

const NOW = Date.parse('2026-10-01T16:30:00Z');
const minDate = NOW - 31 * 86_400_000;
let root: string;

const jsonl = (records: unknown[]) => records.map((r) => JSON.stringify(r)).join('\n') + '\n';
function put(relative: string, records: unknown[]): string {
  const file = path.join(root, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, jsonl(records));
  return file;
}
type Result = Awaited<ReturnType<typeof collectAccountActivity>>;
const total = (data: Result, field: 'inputTokens' | 'outputTokens' | 'cacheReadTokens') =>
  data.hourly.reduce((sum, hour) => sum + hour[field], 0);

/** A Claude Code transcript line for one content block of response `mid`. */
function transcriptLine(mid: string, output: number, at = '2026-10-01T15:00:00Z') {
  return {
    type: 'assistant',
    sessionId: 'sess-t',
    version: '2.1.0',
    cwd: '/work',
    uuid: `u-${mid}-${output}`,
    requestId: `req-${mid}`,
    timestamp: at,
    message: {
      id: mid,
      model: 'claude-opus-5-5',
      usage: {
        input_tokens: 10,
        output_tokens: output,
        cache_read_input_tokens: 1000,
        cache_creation_input_tokens: 5,
      },
    },
  };
}
/** The same response as `claude -p --output-format stream-json` captures it. */
function streamLine(mid: string, output: number, at = '2026-10-01T15:00:01Z') {
  return {
    type: 'assistant',
    session_id: 'sess-s',
    uuid: `su-${mid}-${output}`,
    request_id: `req-${mid}`,
    timestamp: at,
    message: {
      id: mid,
      model: 'claude-opus-5-5',
      usage: {
        input_tokens: 10,
        output_tokens: output,
        cache_read_input_tokens: 1000,
        cache_creation_input_tokens: 5,
      },
    },
  };
}

describe('Claude experiment roots', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-exp-claude-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));
  const projects = () => path.join(root, 'claude', 'projects');
  const experiment = (roots: string[], cache = 'cache', maxBytesPerFile?: number) =>
    collectAccountActivity(
      { kind: 'claude', projectsDir: projects(), experimentRoots: roots },
      { minDate, cacheDir: path.join(root, cache), maxBytesPerFile }
    );
  const standard = (cache = 'cache') =>
    collectAccountActivity(
      { kind: 'claude', projectsDir: projects() },
      { minDate, cacheDir: path.join(root, cache) }
    );

  it('counts nothing for transcript and stream copies of responses the default root holds', async () => {
    const original = [transcriptLine('m1', 10), transcriptLine('m1', 20), transcriptLine('m2', 7)];
    put('claude/projects/p/s.jsonl', original);
    // A byte-identical job copy, and the harness stream capture of the same two responses.
    put('exp/jobs/J1/sessions/s.jsonl', original);
    put('exp/jobs/J1/raw-claude/claude-stream.jsonl', [streamLine('m1', 20), streamLine('m2', 7)]);
    const before = await standard();
    const data = await experiment([
      path.join(root, 'exp/jobs/J1/sessions'),
      path.join(root, 'exp/jobs/J1/raw-claude'),
    ]);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(0);
    expect(data.hourly).toEqual([]);
    // The default request is untouched by the experiment request's reference read.
    const after = await standard();
    expect(after.hourly).toEqual(before.hourly);
    expect(after.eventCount).toBe(2);
    expect(after.scan?.readBytes).toBe(0);
  });

  it('counts a response only an experiment capture holds once, however many copies exist', async () => {
    put('claude/projects/p/s.jsonl', [transcriptLine('m1', 10)]);
    // Two non-identical captures (one with an extra, earlier block) of new response m9.
    put('exp/a/grader/stream.jsonl', [streamLine('m9', 40), streamLine('m9', 90)]);
    put('exp/b/publication/copy/grader/stream.jsonl', [streamLine('m9', 90)]);
    put('exp/b/publication/copy/grader/note.jsonl', [{ hello: 'not usage' }]);
    const data = await experiment([
      path.join(root, 'exp/a/grader'),
      path.join(root, 'exp/b/publication/copy/grader'),
    ]);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(90);
    expect(total(data, 'cacheReadTokens')).toBe(1000);
    // Sessions keep the stream capture's session id.
    expect(data.session).toHaveLength(1);
  });

  it('keeps an experiment response being written pending, counted once (R2-1)', async () => {
    const file = put('exp/a/sessions/s.jsonl', [transcriptLine('m5', 50)]);
    const roots = [path.join(root, 'exp/a/sessions')];
    let data = await experiment(roots);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(50);
    fs.appendFileSync(file, JSON.stringify(transcriptLine('m5', 55)) + '\n');
    data = await experiment(roots);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(55);
  });

  it('reports nothing until the default-root reference read is complete', async () => {
    const lines = Array.from({ length: 40 }, (_, index) => transcriptLine(`m${index}`, index + 1));
    put('claude/projects/p/s.jsonl', lines);
    put('exp/a/sessions/s.jsonl', lines);
    put('exp/a/sessions/t.jsonl', [transcriptLine('new-1', 3)]);
    const roots = [path.join(root, 'exp/a/sessions')];
    let data = await experiment(roots, 'cache', 2048);
    expect(data.scan?.complete).toBe(false);
    expect(data.eventCount).toBe(0);
    for (let pass = 0; pass < 40 && !data.scan?.complete; pass++)
      data = await experiment(roots, 'cache', 2048);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(3);
  });

  it('checks copies against every default Claude root, account instances included (S1)', async () => {
    put('claude/projects/p/s.jsonl', [transcriptLine('m1', 10)]);
    // A response stored under an account instance, and its experiment copy.
    put('instances/work/projects/p/t.jsonl', [transcriptLine('inst-1', 30)]);
    put('exp/jobs/J1/sessions/t.jsonl', [transcriptLine('inst-1', 30), transcriptLine('new-2', 4)]);
    const data = await collectAccountActivity(
      {
        kind: 'claude',
        projectsDir: projects(),
        experimentRoots: [path.join(root, 'exp/jobs/J1/sessions')],
        referenceRoots: [projects(), path.join(root, 'instances/work/projects')],
      },
      { minDate, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(4);
  });

  it('keeps the last good totals while the default root catches up, never a dip (S2)', async () => {
    const original = put('claude/projects/p/s.jsonl', [transcriptLine('m1', 10)]);
    put('exp/a/grader/stream.jsonl', [streamLine('new-1', 40)]);
    const roots = [path.join(root, 'exp/a/grader')];
    let data = await experiment(roots);
    expect(data.scan?.complete).toBe(true);
    expect(total(data, 'outputTokens')).toBe(40);
    // A default transcript in use ends in a half-written line: still caught up, still counted.
    fs.appendFileSync(original, JSON.stringify(transcriptLine('m2', 5)).slice(0, 40));
    data = await experiment(roots);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(40);
    // The default root grows faster than one pass can read: the last good totals stand.
    const more = Array.from({ length: 30 }, (_, index) => transcriptLine(`b${index}`, 1));
    fs.writeFileSync(original, jsonl([transcriptLine('m1', 10), ...more]));
    data = await experiment(roots, 'cache', 1024);
    expect(data.scan?.complete).toBe(false);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(40);
  });

  it('reads a Claude root two levels deep (session subagents) and no deeper, without failing', async () => {
    put('exp/a/sessions/s.jsonl', [transcriptLine('m1', 1)]);
    put('exp/a/sessions/s/subagents/agent-1.jsonl', [transcriptLine('m2', 2)]);
    put('exp/a/sessions/s/subagents/deeper/x.jsonl', [transcriptLine('m3', 4)]);
    const data = await experiment([path.join(root, 'exp/a/sessions')]);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(total(data, 'outputTokens')).toBe(3);
  });
});

function codexMeta(id: string) {
  return { type: 'session_meta', payload: { id, model_provider: 'openai', originator: 'cli' } };
}
function codexTokens(input: number, output: number, at: string) {
  return {
    timestamp: at,
    type: 'event_msg',
    payload: {
      type: 'token_count',
      info: {
        total_token_usage: { input_tokens: input, cached_input_tokens: 0, output_tokens: output },
      },
    },
  };
}

describe('Codex and Muse experiment roots', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-exp-codex-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('counts a Codex record copied across experiment homes once, and drops copies of default logs', async () => {
    const day = 'sessions/2026/10/01';
    const turn = { type: 'turn_context', payload: { model: 'gpt-6-sol' } };
    const first = [codexMeta('c1'), turn, codexTokens(100, 10, '2026-10-01T15:00:00Z')];
    const resumed = [...first, codexTokens(250, 30, '2026-10-01T15:10:00Z')];
    put(`exp/a/private/codex-home/${day}/rollout-2026-10-01T15-00-00-c1.jsonl`, first);
    // A resumed copy under another name: its first event is the same record.
    put(`exp/b/private/codex-home/${day}/rollout-2026-10-01T15-09-00-c1.jsonl`, resumed);
    // A default-root session and its prefix copy in an experiment folder.
    const original = [codexMeta('d1'), turn, codexTokens(70, 7, '2026-10-01T14:00:00Z')];
    put(`codex/${day}/rollout-2026-10-01T14-00-00-d1.jsonl`, [
      ...original,
      codexTokens(90, 9, '2026-10-01T14:30:00Z'),
    ]);
    put(`exp/c/native/codex-copy/${day}/rollout-2026-10-01T14-00-00-d1.jsonl`, original);
    const data = await collectAccountActivity(
      {
        kind: 'codex',
        codexHome: path.join(root, 'codex'),
        cacheDir: path.join(root, 'cache'),
        experimentRoots: [
          path.join(root, 'exp/a/private/codex-home/sessions'),
          path.join(root, 'exp/b/private/codex-home/sessions'),
          path.join(root, 'exp/c/native/codex-copy/sessions'),
        ],
      },
      { minDate, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(total(data, 'inputTokens')).toBe(250);
    expect(total(data, 'outputTokens')).toBe(30);
  });

  it('counts a continued copy of a default Codex session only for its new records (S3)', async () => {
    const day = 'sessions/2026/10/01';
    const id = '01a0f2a8-a754-7233-903b-569c995cf226';
    const turn = { type: 'turn_context', payload: { model: 'gpt-6-sol' } };
    const shared = [
      codexMeta(id),
      turn,
      codexTokens(100, 10, '2026-10-01T14:00:00Z'),
      codexTokens(150, 20, '2026-10-01T14:10:00Z'),
    ];
    put(`codex/${day}/rollout-2026-10-01T14-00-00-${id}.jsonl`, shared);
    // The experiment copy was continued after copying, under another name.
    put(`exp/a/private/codex-home/${day}/rollout-2026-10-01T15-00-00-${id}.jsonl`, [
      ...shared,
      codexTokens(400, 25, '2026-10-01T15:00:00Z'),
    ]);
    const data = await collectAccountActivity(
      {
        kind: 'codex',
        codexHome: path.join(root, 'codex'),
        cacheDir: path.join(root, 'cache'),
        experimentRoots: [path.join(root, 'exp/a/private/codex-home/sessions')],
      },
      { minDate, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'inputTokens')).toBe(250);
    expect(total(data, 'outputTokens')).toBe(5);
  });

  it('counts a continued copy of a default Muse session only for its new records (S3)', async () => {
    const at = Date.parse('2026-10-01T15:00:00Z');
    const usage = (n: number) => ({
      record_type: 'event',
      recorded_at: at + n * 1000,
      id: `rec-${n}`,
      payload: {
        event: {
          kind: 'model_completed',
          model: 'muse-spark-1.3',
          usage: {
            input_tokens: 100,
            output_tokens: n,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
          },
        },
      },
    });
    put('muse/sessions/2026/10/01/01a0-sess/session.jsonl', [usage(1), usage(2)]);
    put('exp/a/private/muse-data/sessions/2026/10/01/01a0-sess/session.jsonl', [
      usage(1),
      usage(2),
      usage(7),
    ]);
    const data = await collectAccountActivity(
      {
        kind: 'muse',
        sessionsDir: path.join(root, 'muse', 'sessions'),
        experimentRoots: [path.join(root, 'exp/a/private/muse-data/sessions')],
      },
      { minDate, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(total(data, 'outputTokens')).toBe(7);
  });

  it('counts a Muse session kept in a private home and its native copy once', async () => {
    const at = Date.parse('2026-10-01T15:00:00Z');
    const usage = (n: number) => ({
      record_type: 'event',
      recorded_at: at + n,
      id: `rec-${n}`,
      payload: {
        event: {
          kind: 'model_completed',
          model: 'muse-spark-1.3',
          usage: {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 40,
            cache_write_tokens: 0,
          },
        },
      },
    });
    const day = 'sessions/2026/10/01/01a0-sess';
    put(`exp/a/private/muse-data/${day}/session.jsonl`, [usage(1), usage(2), usage(3)]);
    put(`exp/a/native/muse-data-copy/${day}/session.jsonl`, [usage(1), usage(2)]);
    const data = await collectAccountActivity(
      {
        kind: 'muse',
        sessionsDir: path.join(root, 'muse', 'sessions'),
        experimentRoots: [
          path.join(root, 'exp/a/private/muse-data/sessions'),
          path.join(root, 'exp/a/native/muse-data-copy/sessions'),
        ],
      },
      { minDate, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(3);
    expect(total(data, 'outputTokens')).toBe(30);
  });
});

describe('zcode experiment databases', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-exp-zcode-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));
  function database(relative: string, inputs: number[]): string {
    const file = path.join(root, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const db = new Database(file);
    db.run(
      'CREATE TABLE IF NOT EXISTS model_usage (model_id TEXT, provider_id TEXT, session_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER)'
    );
    for (const input of inputs)
      db.run('INSERT INTO model_usage VALUES (?, ?, ?, ?, ?, ?, ?, ?)', [
        'GLM-5.3-Flash',
        'zai',
        `s-${relative}`,
        Date.parse('2026-10-01T15:20:00Z'),
        input,
        1,
        0,
        0,
      ]);
    db.close();
    return file;
  }

  const scanZcode = (dbPath: string, experimentDbs: string[]) =>
    collectAccountActivity(
      { kind: 'zcode', dbPath, experimentDbs },
      { minDate, cacheDir: path.join(root, 'cache') }
    );

  it('counts rows shared by continued copies once, default database rows never (S3)', async () => {
    const defaultDb = database('zcode/cli/db/db.sqlite', [7, 8]);
    // An experiment home started from a copy of the default database, then kept running.
    const copy = path.join(root, 'exp/a/zc/cli/db/db.sqlite');
    fs.mkdirSync(path.dirname(copy), { recursive: true });
    fs.copyFileSync(defaultDb, copy);
    database('exp/a/zc/cli/db/db.sqlite', [50]);
    // A second experiment database copied from the first, then continued too.
    const second = path.join(root, 'exp/b/zc/cli/db/db.sqlite');
    fs.mkdirSync(path.dirname(second), { recursive: true });
    fs.copyFileSync(copy, second);
    database('exp/b/zc/cli/db/db.sqlite', [600]);
    const data = await scanZcode(defaultDb, [copy, second]);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(total(data, 'inputTokens')).toBe(650);
  });

  it('keeps the last good rows of a database that cannot be read, and says partial (S4)', async () => {
    const first = database('exp/a/zc/cli/db/db.sqlite', [100]);
    const second = database('exp/b/zc/cli/db/db.sqlite', [20]);
    const defaultDb = path.join(root, 'zcode.sqlite');
    let data = await scanZcode(defaultDb, [first, second]);
    expect(total(data, 'inputTokens')).toBe(120);
    // The second database is rewritten unreadable (a changed, corrupt file).
    const good = fs.readFileSync(second);
    fs.writeFileSync(second, Buffer.alloc(good.length + 4096, 7));
    data = await scanZcode(defaultDb, [first, second]);
    expect(data.scan?.complete).toBe(false);
    expect(total(data, 'inputTokens')).toBe(120);
    // Readable again: read afresh, complete.
    fs.writeFileSync(second, good);
    database('exp/b/zc/cli/db/db.sqlite', [3]);
    data = await scanZcode(defaultDb, [first, second]);
    expect(data.scan?.complete).toBe(true);
    expect(total(data, 'inputTokens')).toBe(123);
    // A database that is gone drops out.
    fs.rmSync(second);
    data = await scanZcode(defaultDb, [first, second]);
    expect(total(data, 'inputTokens')).toBe(100);
  });

  it('keeps each database rows apart, so a change to one never drops or doubles another', async () => {
    const first = database('exp/a/zc/cli/db/db.sqlite', [100]);
    const second = database('exp/b/zc/cli/db/db.sqlite', [20]);
    const scan = () =>
      collectAccountActivity(
        { kind: 'zcode', dbPath: path.join(root, 'zcode.sqlite'), experimentDbs: [first, second] },
        { minDate, cacheDir: path.join(root, 'cache') }
      );
    let data = await scan();
    expect(data.scan?.complete).toBe(true);
    expect(total(data, 'inputTokens')).toBe(120);
    // The second database changes: its rows are replaced, the first's stay.
    database('exp/b/zc/cli/db/db.sqlite', [3]);
    data = await scan();
    expect(total(data, 'inputTokens')).toBe(123);
    // Then the first changes.
    database('exp/a/zc/cli/db/db.sqlite', [1000]);
    data = await scan();
    expect(total(data, 'inputTokens')).toBe(1123);
    expect(data.eventCount).toBe(4);
  });
});

describe('experiment activity requests', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-exp-requests-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('builds one request per tool, leaving out missing roots and roots already read', () => {
    const dir = (relative: string) => {
      fs.mkdirSync(path.join(root, relative), { recursive: true });
      return fs.realpathSync(path.join(root, relative));
    };
    const claudeDefault = dir('claude/projects');
    const jobs = dir('exp/jobs/J1/sessions');
    const codexRoot = dir('exp/a/codex-home/sessions');
    const museRoot = dir('exp/a/muse-data/sessions');
    const db = path.join(dir('exp/a/zc/cli/db'), 'db.sqlite');
    fs.writeFileSync(db, 'x');
    const nested = dir('claude/projects/p');
    const activity = { minDate, cacheDir: path.join(root, 'cache') };
    const requests = experimentActivityRequests(
      {
        claude: [jobs, nested, path.join(root, 'gone')],
        codex: [codexRoot],
        muse: [museRoot],
        zcode: [db, path.join(root, 'gone.sqlite')],
      },
      {
        projectsDir: claudeDefault,
        codexHome: path.join(root, 'codex'),
        sessionsDir: path.join(root, 'muse'),
        dbPath: path.join(root, 'zcode.sqlite'),
      },
      activity,
      path.join(root, 'cache'),
      new Set([`claude:${claudeDefault}`])
    );
    expect(requests.map((entry) => entry.provider)).toEqual(['claude', 'codex', 'muse', 'zcode']);
    expect(requests[0].request).toMatchObject({
      kind: 'claude',
      experimentRoots: [jobs],
      referenceRoots: [claudeDefault],
    });
    expect(requests[1].request).toMatchObject({ kind: 'codex', experimentRoots: [codexRoot] });
    expect(requests[2].request).toMatchObject({ kind: 'muse', experimentRoots: [museRoot] });
    expect(requests[3].request).toMatchObject({ kind: 'zcode', experimentDbs: [db] });
  });
});
