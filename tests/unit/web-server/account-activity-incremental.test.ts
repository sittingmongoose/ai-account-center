import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

// Incremental passes against full ones on the same logs (synthetic data only). Each step changes the logs, then a
// source that reuses its last clean pass where it can, a source that always runs the full pass over the same
// checkpoints, and a brand-new cache that reads everything from scratch must give the same rows.
const DAY = 86_400_000;
let root = '';
let previousCcsHome: string | undefined;
let previousCcsDir: string | undefined;
beforeAll(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-incremental-'));
  previousCcsHome = process.env.CCS_HOME;
  previousCcsDir = process.env.CCS_DIR;
  // the price list is read from the CCS folder: an empty temporary one
  process.env.CCS_HOME = path.join(root, 'home');
  process.env.CCS_DIR = path.join(root, 'home', '.ccs');
  fs.mkdirSync(process.env.CCS_DIR, { recursive: true });
});
afterAll(() => {
  if (previousCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousCcsHome;
  if (previousCcsDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = previousCcsDir;
  fs.rmSync(root, { recursive: true, force: true });
});

const MODELS = ['claude-opus-4-7', 'claude-sonnet-4-6', 'claude-haiku-4-5'];
let seq = 0;
function claudeLines(sessionId: string, start: number, count: number): string {
  const lines: string[] = [];
  for (let i = 0; i < count; i++) {
    seq++;
    const timestamp = new Date(start + i * 45_000).toISOString();
    lines.push(
      JSON.stringify({
        type: 'user',
        sessionId,
        timestamp,
        message: { role: 'user', content: 'synthetic prompt' },
      })
    );
    lines.push(
      JSON.stringify({
        type: 'assistant',
        uuid: `a-${seq}`,
        requestId: `req_${seq}`,
        sessionId,
        timestamp,
        message: {
          id: `msg_${seq}`,
          model: MODELS[seq % 3],
          role: 'assistant',
          content: [{ type: 'text', text: 'synthetic answer' }],
          usage: {
            input_tokens: 10 + (seq % 97),
            output_tokens: 5 + (seq % 31),
            cache_read_input_tokens: seq % 500,
            cache_creation_input_tokens: seq % 70,
          },
        },
      })
    );
  }
  return lines.join('\n') + '\n';
}

function codexLines(sessionId: string, start: number, count: number): string {
  const lines = [
    JSON.stringify({
      timestamp: new Date(start).toISOString(),
      type: 'session_meta',
      payload: {
        id: sessionId,
        timestamp: new Date(start).toISOString(),
        cwd: '/work',
        cli_version: '0.126.0',
        source: 'cli',
        model_provider: 'openai',
      },
    }),
    JSON.stringify({
      timestamp: new Date(start + 1000).toISOString(),
      type: 'turn_context',
      payload: { turn_id: 't1', cwd: '/work', model: 'gpt-5', effort: 'high' },
    }),
  ];
  const total = {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: 0,
  };
  for (let i = 0; i < count; i++) {
    const last = {
      input_tokens: 100 + i,
      cached_input_tokens: i,
      output_tokens: 10 + i,
      reasoning_output_tokens: 1,
      total_tokens: 110 + 2 * i,
    };
    for (const key of Object.keys(total) as Array<keyof typeof total>) total[key] += last[key];
    lines.push(
      JSON.stringify({
        timestamp: new Date(start + 2000 + i * 50_000).toISOString(),
        type: 'event_msg',
        payload: {
          type: 'token_count',
          info: {
            total_token_usage: { ...total },
            last_token_usage: last,
            model_context_window: 272000,
          },
        },
      })
    );
  }
  return lines.join('\n') + '\n';
}

type Result = Awaited<ReturnType<typeof collectAccountActivity>>;
/** What the page is built from: the hourly and session rows and the event count. */
const rows = (result: Result) => ({
  hourly: result.hourly,
  session: result.session,
  eventCount: result.eventCount,
  complete: result.scan?.complete,
});
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

describe('incremental analytics passes', () => {
  for (const kind of ['claude', 'codex'] as const) {
    it(`${kind}: reusing unchanged passes and re-reading changed logs equals a full read, step by step`, async () => {
      const base = path.join(root, kind);
      const logs =
        kind === 'claude' ? path.join(base, 'projects') : path.join(base, 'codex-home', 'sessions');
      const request =
        kind === 'claude'
          ? { kind: 'claude' as const, projectsDir: logs }
          : { kind: 'codex' as const, codexHome: path.join(base, 'codex-home') };
      const now = Date.now();
      const write = (name: string, text: string, append = false) => {
        const file = path.join(
          logs,
          kind === 'claude' ? `-work-p${name.length % 3}` : '2026/11/13',
          name
        );
        fs.mkdirSync(path.dirname(file), { recursive: true });
        if (append) fs.appendFileSync(file, text);
        else fs.writeFileSync(file, text);
        return file;
      };
      const lines = (id: string, start: number, count: number) =>
        kind === 'claude' ? claudeLines(id, start, count) : codexLines(id, start, count);
      const name = (n: number) =>
        kind === 'claude'
          ? `00000000-0000-4000-8000-${String(n).padStart(12, '0')}.jsonl`
          : `rollout-2026-11-13T00-00-00-0199${String(n).padStart(4, '0')}-0000-7000-8000-${String(n).padStart(12, '0')}.jsonl`;
      const files: string[] = [];
      for (let n = 0; n < 12; n++)
        files.push(write(name(n), lines(`s${n}`, now - (n + 1) * DAY, 20 + n)));

      let minDate = now - 31 * DAY;
      const incremental = path.join(base, 'cache-incremental');
      const full = path.join(base, 'cache-full');
      let fresh = 0;
      const pass = (cacheDir: string, reuse: boolean) =>
        collectAccountActivity(request, { minDate, cacheDir }, { reuse, racyMs: 0 });
      /** One step: both sources and a brand-new cache read the same logs; returns the incremental result. */
      const step = async (label: string, reused: boolean) => {
        await sleep(5);
        if (reused) {
          // proof that a reused pass loads no checkpoint: a damaged one would be read again from the log's start
          const dir = path.join(incremental, 'account-activity-v1');
          const source = path.join(dir, fs.readdirSync(dir)[0]);
          const checkpoint = fs
            .readdirSync(source)
            .find((entry) => /^[a-f0-9]{64}\.json$/.test(entry));
          fs.writeFileSync(path.join(source, checkpoint as string), '{"damaged":');
        }
        const a = await pass(incremental, true);
        const b = await pass(full, false);
        const c = await pass(path.join(base, `cache-fresh-${fresh++}`), true);
        expect(rows(a), `${label}: incremental vs full`).toEqual(rows(b));
        expect(rows(a), `${label}: incremental vs a read from scratch`).toEqual(rows(c));
        expect(a.scan?.complete, label).toBe(true);
        // a reused pass reads nothing and returns the last result as it was; a full one walks every checkpoint
        if (reused) expect(a.scan?.readBytes, `${label}: reused`).toBe(0);
        last = b;
        return a;
      };
      let last: Result | null = null;

      const first = await step('first read', false);
      expect(first.eventCount).toBeGreaterThan(0);
      const unchanged = await step('nothing changed', true);
      expect(unchanged).toEqual({ ...first, scan: { ...first.scan!, readBytes: 0 } });

      // appended lines: only the new bytes are read, and they are counted once
      write(name(0), lines('s0', now - 30 * 60_000, 7), true);
      const appended = await step('appended to one log', false);
      // (a Codex log's appended turn restarts its running totals, so its first new count is a reset, not usage)
      expect(appended.eventCount).toBe(first.eventCount + (kind === 'claude' ? 7 : 6));
      // the full source read only the appended bytes of that log (its checkpoint held the rest)
      expect(last!.scan?.readBytes).toBeGreaterThan(0);
      expect(last!.scan?.readBytes).toBeLessThan(fs.statSync(files[0]).size / 2);
      await step('nothing changed after the append', true);

      // a new log
      files.push(write(name(40), lines('s40', now - 20 * 60_000, 9)));
      const added = await step('a new log', false);
      expect(added.eventCount).toBe(appended.eventCount + 9);

      // a log rewritten shorter (truncated): rebuilt from its new content, never added to its old counts
      const truncated = files[3];
      const kept = fs.readFileSync(truncated, 'utf8').split('\n').filter(Boolean);
      fs.writeFileSync(truncated, kept.slice(0, kind === 'claude' ? 10 : 7).join('\n') + '\n');
      const shorter = await step('a truncated log', false);
      expect(shorter.eventCount).toBeLessThan(added.eventCount);

      // a deleted log leaves the totals
      fs.rmSync(files[5]);
      const deleted = await step('a deleted log', false);
      expect(deleted.eventCount).toBeLessThan(shorter.eventCount);

      // a rotated log: the same path, a new file (new inode) with other content
      const rotated = files[6];
      fs.renameSync(rotated, `${rotated}.old`);
      fs.rmSync(`${rotated}.old`);
      fs.writeFileSync(rotated, lines('s6b', now - 3 * 60 * 60_000, 4));
      await step('a rotated log', false);

      // the moving window drops the oldest rows: an unchanged pass that would keep them is not reused
      await step('settled again', true);
      minDate = now - 11.5 * DAY;
      const narrowed = await step('the window moved past the oldest rows', false);
      expect(narrowed.eventCount).toBeLessThan(deleted.eventCount);

      // a new price list prices the same rows again: the unchanged pass is not reused (a damaged checkpoint is then
      // read again from its log's start, which a reused pass would never do)
      await step('settled before the price list', true);
      fs.writeFileSync(
        path.join(process.env.CCS_DIR as string, 'models-dev-registry-cache.json'),
        JSON.stringify({ version: 1, fetchedAt: Date.now(), providers: {} })
      );
      const dir = path.join(incremental, 'account-activity-v1');
      const source = path.join(dir, fs.readdirSync(dir)[0]);
      for (const entry of fs.readdirSync(source).filter((e) => /^[a-f0-9]{64}\.json$/.test(e)))
        fs.writeFileSync(path.join(source, entry), '{"damaged":');
      const repriced = await pass(incremental, true);
      expect(repriced.scan?.readBytes).toBeGreaterThan(0);
      expect(rows(repriced)).toEqual(rows(await pass(full, false)));
      fs.rmSync(path.join(process.env.CCS_DIR as string, 'models-dev-registry-cache.json'));
    });
  }

  it('a log changed right before the walk is not trusted to be settled', async () => {
    const base = path.join(root, 'racy');
    const logs = path.join(base, 'projects', '-work');
    fs.mkdirSync(logs, { recursive: true });
    const file = path.join(logs, '00000000-0000-4000-8000-000000000001.jsonl');
    fs.writeFileSync(file, claudeLines('r1', Date.now() - DAY, 5));
    const cacheDir = path.join(base, 'cache');
    const request = { kind: 'claude' as const, projectsDir: path.join(base, 'projects') };
    const options = { minDate: Date.now() - 31 * DAY, cacheDir };
    await collectAccountActivity(request, options);
    // the default racy window (2 s): the log was written just now, so the summary is not used yet
    const summaries = path.join(cacheDir, 'account-activity-passes-v1');
    expect(fs.readdirSync(summaries).length).toBe(1);
    const summary = JSON.parse(
      fs.readFileSync(path.join(summaries, fs.readdirSync(summaries)[0]), 'utf8')
    ) as { newestChangeAt: number; walkedAt: number; result: UsageWorkerResult };
    expect(summary.newestChangeAt).toBeGreaterThanOrEqual(summary.walkedAt - 2_000);
    // the summary holds rows and counts only, never a project path
    expect(JSON.stringify(summary.result)).not.toContain('/work');
  });
});
