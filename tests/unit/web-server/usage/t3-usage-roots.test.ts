import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../../src/web-server/usage/account-activity-collector';
import { emptyRootSet } from '../../../../src/web-server/usage/experiment-usage-roots';
import {
  filesUnderT3Root,
  resolveT3UsageRoots,
  T3_MAX_HOMES,
} from '../../../../src/web-server/usage/t3-usage-roots';
import {
  experimentActivityRequests,
  type AccountAnalyticsActivityRequest,
} from '../../../../src/web-server/services/account-analytics-activity';

const NOW = Date.parse('2026-10-01T16:30:00Z');
const minDate = NOW - 31 * 86_400_000;
let home: string;

const jsonl = (records: unknown[]) => records.map((r) => JSON.stringify(r)).join('\n') + '\n';
function put(relative: string, records: unknown[]): string {
  const file = path.join(home, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, jsonl(records));
  return file;
}
function link(target: string, relative: string): string {
  const at = path.join(home, relative);
  fs.mkdirSync(path.dirname(at), { recursive: true });
  fs.symlinkSync(path.isAbsolute(target) ? target : path.join(home, target), at);
  return at;
}
type Result = Awaited<ReturnType<typeof collectAccountActivity>>;
const sum = (results: Result[], field: 'inputTokens' | 'outputTokens' | 'cacheReadTokens') =>
  results.reduce((all, data) => all + data.hourly.reduce((s, hour) => s + hour[field], 0), 0);
const events = (results: Result[]) => results.reduce((all, data) => all + data.eventCount, 0);

function transcriptLine(mid: string, output: number, session = 'sess-t') {
  return {
    type: 'assistant',
    sessionId: session,
    version: '2.1.0',
    cwd: '/work',
    uuid: `u-${mid}-${output}`,
    requestId: `req-${mid}`,
    timestamp: '2026-10-01T15:00:00Z',
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
const codexMeta = (id: string) => ({
  type: 'session_meta',
  payload: { id, model_provider: 'openai', originator: 'cli' },
});
const turn = { type: 'turn_context', payload: { model: 'gpt-6-sol' } };
const codexTokens = (input: number, output: number, at: string) => ({
  timestamp: at,
  type: 'event_msg',
  payload: {
    type: 'token_count',
    info: {
      total_token_usage: { input_tokens: input, cached_input_tokens: 0, output_tokens: output },
    },
  },
});
const ID_A = '01a0f2a8-a754-7233-903b-569c995cf226';
const ID_B = '02b1f2a8-a754-7233-903b-569c995cf227';
const ID_NEW = '03c2f2a8-a754-7233-903b-569c995cf228';

/** Two default Codex sessions under `~/.codex/sessions`. */
function defaultCodex(): void {
  const day = '.codex/sessions/2026/10/01';
  put(`${day}/rollout-2026-10-01T14-00-00-${ID_A}.jsonl`, [
    codexMeta(ID_A),
    turn,
    codexTokens(100, 10, '2026-10-01T14:00:00Z'),
    codexTokens(150, 20, '2026-10-01T14:10:00Z'),
  ]);
  put(`${day}/rollout-2026-10-01T15-00-00-${ID_B}.jsonl`, [
    codexMeta(ID_B),
    turn,
    codexTokens(40, 4, '2026-10-01T15:00:00Z'),
  ]);
}

/** The app's local plan for this fixture home: the default requests plus the T3/experiment ones. */
function plan(): AccountAnalyticsActivityRequest[] {
  const activity = { minDate, cacheDir: path.join(home, 'cache') };
  const projectsDir = path.join(home, '.claude', 'projects');
  const codexHome = path.join(home, '.codex');
  const scanned = new Set<string>();
  const requests: AccountAnalyticsActivityRequest[] = [];
  if (fs.existsSync(projectsDir)) {
    scanned.add(`claude:${fs.realpathSync(projectsDir)}`);
    requests.push({ provider: 'claude', request: { kind: 'claude', projectsDir, activity } });
  }
  if (fs.existsSync(path.join(codexHome, 'sessions'))) {
    scanned.add(`codex:${fs.realpathSync(path.join(codexHome, 'sessions'))}`);
    requests.push({
      provider: 'codex',
      request: { kind: 'codex', codexHome, cacheDir: activity.cacheDir, activity },
    });
  }
  requests.push(
    ...experimentActivityRequests(
      emptyRootSet(),
      {
        projectsDir,
        codexHome,
        sessionsDir: path.join(home, 'muse'),
        dbPath: path.join(home, 'zcode.sqlite'),
      },
      activity,
      activity.cacheDir,
      scanned,
      resolveT3UsageRoots({ homeDir: home })
    )
  );
  return requests;
}
async function run(requests: AccountAnalyticsActivityRequest[], provider: string) {
  const results: Result[] = [];
  for (const entry of requests.filter((item) => item.provider === provider)) {
    const request = entry.request as Parameters<typeof collectAccountActivity>[0];
    results.push(
      await collectAccountActivity(request, { minDate, cacheDir: path.join(home, 'cache') })
    );
  }
  return results;
}

beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-t3-usage-'));
});
afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

describe('T3 Code usage roots', () => {
  it('finds every T3 Claude config dir and Codex home, by any folder name, plus the settings homes', () => {
    for (const account of ['alpha', 'beta', 'gamma', 'delta'])
      fs.mkdirSync(path.join(home, '.claude-t3', account, 'projects'), { recursive: true });
    fs.mkdirSync(path.join(home, '.claude-t3', 'no-history'), { recursive: true });
    fs.mkdirSync(path.join(home, '.codex', 'sessions'), { recursive: true });
    for (const account of ['alpha', 'beta', 'gamma'])
      link('.codex/sessions', `.codex-t3/${account}/sessions`);
    link('.codex/sessions', '.t3/userdata/providers/codex/codex_1/shadow/sessions');
    fs.mkdirSync(path.join(home, 'elsewhere', 'claude-x', 'projects'), { recursive: true });
    fs.mkdirSync(path.join(home, 'elsewhere', 'codex-y', 'sessions'), { recursive: true });
    fs.writeFileSync(
      path.join(home, '.t3', 'userdata', 'settings.json'),
      JSON.stringify({
        providerInstances: {
          claudeAgent: { driver: 'claudeAgent', config: { homePath: '' } },
          claude_x: { driver: 'claudeAgent', config: { homePath: '~/elsewhere/claude-x' } },
          codex_y: {
            driver: 'codex',
            config: { homePath: '~/.codex', shadowHomePath: '~/elsewhere/codex-y' },
          },
          relative: { driver: 'claudeAgent', config: { homePath: 'not/absolute' } },
          other: { driver: 'grok', config: { homePath: '~/elsewhere/claude-x' } },
        },
      })
    );
    const roots = resolveT3UsageRoots({ homeDir: home });
    expect(roots.claude).toEqual([
      ...['alpha', 'beta', 'delta', 'gamma'].map((a) =>
        path.join(home, '.claude-t3', a, 'projects')
      ),
      path.join(home, 'elsewhere', 'claude-x', 'projects'),
    ]);
    // Every shadow link resolves to ~/.codex/sessions: one entry for all of them (the first).
    expect(roots.codex).toEqual([
      path.join(home, '.codex-t3', 'alpha', 'sessions'),
      path.join(home, 'elsewhere', 'codex-y', 'sessions'),
    ]);
  });

  it('finds nothing, and reads no settings, when T3 Code is not set up', () => {
    expect(resolveT3UsageRoots({ homeDir: home })).toEqual({ claude: [], codex: [] });
    fs.mkdirSync(path.join(home, '.t3', 'userdata'), { recursive: true });
    fs.writeFileSync(path.join(home, '.t3', 'userdata', 'settings.json'), '{not json');
    expect(resolveT3UsageRoots({ homeDir: home })).toEqual({ claude: [], codex: [] });
  });

  it('bounds the account folders read per base', () => {
    for (let index = 0; index < T3_MAX_HOMES + 5; index++)
      fs.mkdirSync(
        path.join(home, '.claude-t3', `a${String(index).padStart(3, '0')}`, 'projects'),
        {
          recursive: true,
        }
      );
    expect(resolveT3UsageRoots({ homeDir: home }).claude).toHaveLength(T3_MAX_HOMES);
  });

  it('follows links inside a T3 root once, never into an excluded root, and ends on a loop', () => {
    put('.codex/sessions/2026/10/01/rollout-a.jsonl', [codexMeta(ID_A)]);
    const own = put('.codex-t3/alpha/sessions/2026/10/02/rollout-b.jsonl', [codexMeta(ID_B)]);
    put('outside/2026/10/03/rollout-c.jsonl', [codexMeta(ID_NEW)]);
    link('.codex/sessions/2026', '.codex-t3/alpha/sessions/linked-default');
    link('outside', '.codex-t3/alpha/sessions/linked-outside');
    link('outside', '.codex-t3/alpha/sessions/linked-outside-again');
    link('.codex-t3/alpha/sessions', '.codex-t3/alpha/sessions/2026/loop');
    link('.codex-t3/alpha/sessions/missing', '.codex-t3/alpha/sessions/dangling');
    const walked = filesUnderT3Root(
      path.join(home, '.codex-t3/alpha/sessions'),
      (name) => name.startsWith('rollout-'),
      [path.join(home, '.codex', 'sessions')],
      {
        deadline: Date.now() + 10_000,
        maxDepth: 64,
        maxDirectories: 1000,
        maxEntries: 1000,
        maxFiles: 100,
      }
    );
    expect(walked.truncated).toBe(false);
    expect(walked.files.sort()).toEqual(
      [
        fs.realpathSync(own),
        fs.realpathSync(path.join(home, 'outside/2026/10/03/rollout-c.jsonl')),
      ].sort()
    );
  });
});

describe('T3 Code Codex shadow homes are never double counted', () => {
  it('a shadow home whose sessions link to ~/.codex/sessions adds nothing: totals equal the single home', async () => {
    defaultCodex();
    const single = await run(plan(), 'codex');
    fs.rmSync(path.join(home, 'cache'), { recursive: true, force: true });
    for (const account of ['alpha', 'beta', 'gamma'])
      link('.codex/sessions', `.codex-t3/${account}/sessions`);
    link('.codex/sessions', '.t3/userdata/providers/codex/codex_1/shadow/sessions');
    const requests = plan();
    // The links resolve into the default root, so no second Codex request is made...
    expect(requests.filter((entry) => entry.provider === 'codex')).toHaveLength(1);
    const withT3 = await run(requests, 'codex');
    expect(events(withT3)).toBe(events(single));
    expect(sum(withT3, 'inputTokens')).toBe(sum(single, 'inputTokens'));
    expect(sum(withT3, 'outputTokens')).toBe(sum(single, 'outputTokens'));
    expect(events(single)).toBe(3);
    // ...and even read directly, the shadow root yields no record: its links point into the default.
    const direct = await collectAccountActivity(
      {
        kind: 'codex',
        codexHome: path.join(home, '.codex'),
        cacheDir: path.join(home, 'cache'),
        homeRoots: [path.join(home, '.codex-t3', 'alpha', 'sessions')],
      },
      { minDate, cacheDir: path.join(home, 'cache') }
    );
    expect(direct.scan?.complete).toBe(true);
    expect(direct.eventCount).toBe(0);
  });

  it('a real shadow folder of links, hard links and copies counts only its own new session', async () => {
    defaultCodex();
    const single = await run(plan(), 'codex');
    fs.rmSync(path.join(home, 'cache'), { recursive: true, force: true });
    const shadow = '.codex-t3/beta/sessions';
    link('.codex/sessions/2026', `${shadow}/2026`);
    // A hard link and a byte copy of default session A, and a link loop.
    fs.mkdirSync(path.join(home, shadow, 'kept'), { recursive: true });
    const original = path.join(
      home,
      `.codex/sessions/2026/10/01/rollout-2026-10-01T14-00-00-${ID_A}.jsonl`
    );
    fs.linkSync(original, path.join(home, shadow, 'kept', path.basename(original)));
    fs.copyFileSync(original, path.join(home, shadow, 'kept', `rollout-copy-${ID_A}.jsonl`));
    link(shadow, `${shadow}/kept/loop`);
    // One session that only this account's home holds.
    put(`${shadow}/own/rollout-2026-10-01T16-00-00-${ID_NEW}.jsonl`, [
      codexMeta(ID_NEW),
      turn,
      codexTokens(500, 50, '2026-10-01T16:00:00Z'),
    ]);
    const requests = plan();
    const codex = requests.filter((entry) => entry.provider === 'codex');
    expect(codex).toHaveLength(2);
    expect(codex[1].request).toMatchObject({
      kind: 'codex',
      experimentRoots: [],
      homeRoots: [fs.realpathSync(path.join(home, shadow))],
    });
    const withT3 = await run(requests, 'codex');
    expect(withT3[1].scan?.complete).toBe(true);
    expect(events(withT3)).toBe(events(single) + 1);
    expect(sum(withT3, 'inputTokens')).toBe(sum(single, 'inputTokens') + 500);
    expect(sum(withT3, 'outputTokens')).toBe(sum(single, 'outputTokens') + 50);
  });
});

describe('T3 Code Claude config dirs', () => {
  it('counts each T3 transcript once: new responses count, copies of default and other T3 transcripts do not', async () => {
    put('.claude/projects/p/s1.jsonl', [transcriptLine('m1', 10), transcriptLine('m1', 20)]);
    const single = await run(plan(), 'claude');
    fs.rmSync(path.join(home, 'cache'), { recursive: true, force: true });
    // alpha: its own new session (two blocks of one response) plus a non-identical copy of s1.
    put('.claude-t3/alpha/projects/p/s2.jsonl', [
      transcriptLine('m2', 5, 's2'),
      transcriptLine('m2', 7, 's2'),
    ]);
    put('.claude-t3/alpha/projects/p/s1.jsonl', [transcriptLine('m1', 20)]);
    // beta holds a copy of alpha's new session in a subagent folder (full depth is read).
    put('.claude-t3/beta/projects/q/s2/subagents/agent-1.jsonl', [transcriptLine('m2', 7, 's2')]);
    // gamma's projects folder is a link to the default root: left out.
    fs.mkdirSync(path.join(home, '.claude-t3', 'gamma'), { recursive: true });
    link('.claude/projects', '.claude-t3/gamma/projects');
    // delta keeps a session subagent three levels down.
    put('.claude-t3/delta/projects/r/s3/subagents/agent-2.jsonl', [transcriptLine('m3', 9, 's3')]);
    const requests = plan();
    const claude = requests.filter((entry) => entry.provider === 'claude');
    expect(claude).toHaveLength(2);
    expect(claude[1].request).toMatchObject({
      kind: 'claude',
      experimentRoots: [],
      homeRoots: ['alpha', 'beta', 'delta'].map((a) =>
        fs.realpathSync(path.join(home, '.claude-t3', a, 'projects'))
      ),
      referenceRoots: [fs.realpathSync(path.join(home, '.claude', 'projects'))],
    });
    const withT3 = await run(requests, 'claude');
    expect(withT3[1].scan?.complete).toBe(true);
    expect(events(withT3)).toBe(events(single) + 2);
    expect(sum(withT3, 'outputTokens')).toBe(sum(single, 'outputTokens') + 7 + 9);
    expect(sum(withT3, 'cacheReadTokens')).toBe(sum(single, 'cacheReadTokens') + 2000);
  });

  it('changes nothing when no T3 home exists', async () => {
    put('.claude/projects/p/s1.jsonl', [transcriptLine('m1', 10)]);
    defaultCodex();
    const requests = plan();
    expect(requests.map((entry) => entry.provider)).toEqual(['claude', 'codex']);
  });
});
