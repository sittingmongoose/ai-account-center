/**
 * Scoreboard v2 with the real history store (dist/native, `bun run store:build`): runs are
 * ingested from a temporary share into a temporary store, kept, filtered in the store and
 * normalized there; the secret filter refuses before anything is stored. Skipped, and said so,
 * when this computer has no module. Never the real share or home.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { loadNativeStore, nativeModuleName } from '../../../../src/store/native-loader';
import { AacStore } from '../../../../src/store/store-service';
import {
  ALL_RUNS,
  ScoreboardService,
} from '../../../../src/web-server/scoreboard/scoreboard-service';
import {
  NAME_WITHHELD,
  pathHash,
  RunsStore,
  SECRET_NAME_REASON,
  type RefusedRow,
} from '../../../../src/web-server/scoreboard/runs-store';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import zlib from 'zlib';
import { NOW } from '../status/status-test-helpers';
import { runDoc, writeRun } from './scoreboard-test-helpers';

const MODULE = path.resolve(__dirname, '../../../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });
const haveModule = loaded.module !== null;
if (!haveModule)
  console.warn(`[!] ${MODULE} is not built: the Scoreboard store tests are skipped.`);

const CATALOG = path.resolve(__dirname, '../../../../docs/agent-reports/catalog');
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const iso = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z');

let sandbox = '';
let root = '';
let store: AacStore | null = null;

function share(): string {
  const dir = path.join(sandbox, 'share');
  fs.mkdirSync(path.join(dir, 'v1', 'catalog'), { recursive: true });
  for (const name of fs.readdirSync(CATALOG))
    fs.copyFileSync(path.join(CATALOG, name), path.join(dir, 'v1', 'catalog', name));
  return dir;
}

function put(local: string, ended: number, fields: Record<string, unknown> = {}) {
  const doc = runDoc('vm.lead', local, { endedAt: iso(ended), writtenAt: iso(ended), ...fields });
  writeRun(root, doc, iso(ended).slice(0, 10));
  return doc;
}

function service(withStore = true) {
  return new ScoreboardService({
    root: () => root,
    now: () => NOW,
    timeZone: () => 'UTC',
    native: null,
    store: () => (withStore ? store : null),
  });
}

beforeAll(async () => {
  if (!haveModule) return;
  sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-scoreboard-store-'));
  root = share();
  // Six Sol 6.1 builds at "X-High", each with a later blind review by Opus.
  for (let i = 0; i < 6; i++) {
    put(`b${i}`, NOW - (10 - i) * DAY, {
      model: { provider: 'party', id: 'gpt-6.1-sol', harness: 'claude-code', effort: 'X-High' },
    });
    put(`rv${i}`, NOW - (10 - i) * DAY + 2 * HOUR, {
      task: { id: `rv${i}`, type: 'review', title: `Review ${i}` },
      model: {
        provider: 'anthropic',
        id: 'claude-opus-5-5',
        harness: 't3-delegate',
        effort: 'high',
      },
      review: {
        of: `vm.lead--b${i}`,
        verdict: 'pass',
        findings: { high: 0, medium: 0, low: 0 },
        overturned: false,
      },
    });
  }
  put('qwen', NOW - 3 * DAY, {
    model: { provider: 'omp', id: 'alibaba-token-plan/qwen3.8-max', harness: 'omp', effort: null },
  });
  put('bench', NOW - 2 * DAY, { source: 'benchmark', benchmark: 'pilot' });
  // A credential in a title: refused by the secret filter, never stored.
  put('secret', NOW - DAY, {
    task: {
      id: 'secret',
      type: 'build',
      title: 'uses sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123',
    },
  });
  const handle = new loaded.module!.NativeStore();
  await handle.call('open', JSON.stringify({ dir: path.join(sandbox, 'store') }));
  store = new AacStore(handle, path.join(sandbox, 'store'));
});

afterAll(async () => {
  if (store) await store.close();
  if (sandbox) fs.rmSync(sandbox, { recursive: true, force: true });
});

describe.skipIf(!haveModule)('Scoreboard with the history store', () => {
  it('keeps every accepted run, normalized, and refuses a secret before storing it', async () => {
    const runs = new RunsStore({ store: () => store, root: () => root, now: () => NOW });
    const pass = await runs.pass({ full: true });
    expect(pass.recorded).toBe(14);
    expect(pass.refused).toBe(1);
    const again = await runs.pass({ full: true });
    expect(again.recorded).toBe(0);
    const stored = await runs.query({});
    expect(stored.map((run) => run.doc.id)).not.toContain('vm.lead--secret');
    const sol = stored.find((run) => run.doc.id === 'vm.lead--b0')!;
    expect(sol.doc.model).toMatchObject({
      id: 'openai/gpt-6.1-sol',
      effort: 'xhigh',
      harness: 'codex',
    });
    expect(sol.identity).toMatchObject({ harnessSource: 'corrected', modelFamily: 'openai' });
    const qwen = stored.find((run) => run.doc.id === 'vm.lead--qwen')!;
    expect(qwen.doc.model).toMatchObject({ id: 'alibaba/qwen3.8-max', effort: null });
    // A refusal is kept by the path's hash alone: no path, name or writer.
    const refused = await store!.runs<RefusedRow[]>('runsRefused');
    expect(refused).toEqual([
      expect.objectContaining({
        pathHash: pathHash(
          'writers/vm.lead/runs/' + iso(NOW - DAY).slice(0, 10) + '/vm.lead--secret.json'
        ),
        reason: 'refused: it contains what looks like an API key',
      }),
    ]);
    expect(JSON.stringify(refused)).not.toContain('vm.lead--secret');
    // The Sources name it by the path this server saw, which the secret filter passes.
    expect((await runs.state({})).problems.map((problem) => problem.file)).toContain(
      'writers/vm.lead/runs/' + iso(NOW - DAY).slice(0, 10) + '/vm.lead--secret.json'
    );
  });

  it('serves the view from the store with filters, identity and history', async () => {
    const scoreboard = service();
    const all = await scoreboard.view(ALL_RUNS);
    expect(all.filters).toMatchObject({ available: true, source: 'all' });
    expect(all.history).toEqual({ store: 'on', runs: 14, importing: false });
    expect(all.identity?.families.map((family) => family.id).slice(0, 2)).toEqual([
      'anthropic',
      'openai',
    ]);
    const builds = all.uses
      .flatMap((use) => use.taskTypes)
      .find((type) => type.id === 'build')!
      .rows.find((row) => row.model === 'openai/gpt-6.1-sol')!;
    expect(builds).toMatchObject({ runtime: 'codex', effort: 'xhigh' });
    expect(builds.observed?.measures.quality.of).toBe(6);
    // Only build runs: the reviews still join, so quality keeps its six passed items.
    const buildOnly = await scoreboard.view({ ...ALL_RUNS, taskType: 'build' });
    const reviewRows = buildOnly.uses
      .flatMap((use) => use.taskTypes)
      .filter((type) => type.id === 'review')
      .flatMap((type) => type.rows);
    expect(reviewRows).toEqual([]);
    const row = buildOnly.uses
      .flatMap((use) => use.taskTypes)
      .find((type) => type.id === 'build')!
      .rows.find((r) => r.model === 'openai/gpt-6.1-sol')!;
    expect(row.observed?.measures.quality.of).toBe(6);
    // The last three days only.
    const recent = await scoreboard.view({
      ...ALL_RUNS,
      from: iso(NOW - 3 * DAY).slice(0, 10),
      to: iso(NOW).slice(0, 10),
    });
    const recentRuns = recent.uses.flatMap((use) =>
      use.taskTypes.flatMap((type) => type.rows.map((r) => r.n.observed + r.n.benchmark))
    );
    expect(recentRuns.reduce((a, b) => a + b, 0)).toBeLessThan(
      all.uses
        .flatMap((use) =>
          use.taskTypes.flatMap((type) => type.rows.map((r) => r.n.observed + r.n.benchmark))
        )
        .reduce((a, b) => a + b, 0)
    );
    const bench = await scoreboard.view({ ...ALL_RUNS, source: 'benchmark' });
    const benchRows = bench.uses.flatMap((use) => use.taskTypes.flatMap((type) => type.rows));
    expect(benchRows.every((r) => r.n.observed === 0)).toBe(true);
  });

  it('without the store keeps the share-only view and says filters are unavailable', async () => {
    const view = await service(false).view(ALL_RUNS);
    expect(view.filters?.available).toBe(false);
    expect(view.history).toEqual({ store: 'off', runs: null, importing: false });
  });
});

// ---------------------------------------------------------------------------------------------
// Review 1 regressions with the real module: H1 (no credential in the store), M1 and M2 (the
// store's winner is the TypeScript reader's). Synthetic values only.

/** The reviewer's synthetic witness (never a real credential). */
const WITNESS = 'sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123';

/** Files under `dir` holding `needle`, compressed segments decoded. */
function holding(dir: string, needle: string): string[] {
  const hits: string[] = [];
  const walk = (at: string) => {
    for (const entry of fs.readdirSync(at, { withFileTypes: true })) {
      const file = path.join(at, entry.name);
      if (entry.isDirectory()) walk(file);
      else {
        let bytes = fs.readFileSync(file);
        if (file.endsWith('.zst')) bytes = zlib.zstdDecompressSync(bytes);
        if (bytes.includes(Buffer.from(needle))) hits.push(file);
      }
    }
  };
  walk(dir);
  return hits;
}

async function openStore(dir: string): Promise<AacStore> {
  const handle = new loaded.module!.NativeStore();
  await handle.call('open', JSON.stringify({ dir }));
  return new AacStore(handle, dir);
}

describe.skipIf(!haveModule)('Scoreboard v2 review regressions', () => {
  it('H1: a credential in a run file name reaches neither the DB nor the ledger', async () => {
    const box = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-sb-h1-'));
    try {
      const shareRoot = path.join(box, 'share');
      fs.mkdirSync(path.join(shareRoot, 'v1', 'catalog'), { recursive: true });
      for (const name of fs.readdirSync(CATALOG))
        fs.copyFileSync(path.join(CATALOG, name), path.join(shareRoot, 'v1', 'catalog', name));
      const day = iso(NOW - DAY).slice(0, 10);
      const dir = path.join(shareRoot, 'v1', 'writers', 'vm.lead', 'runs', day);
      fs.mkdirSync(dir, { recursive: true });
      // The reviewer's witness: the credential as the file name and the run id.
      const named = runDoc('vm.lead', WITNESS, {
        endedAt: iso(NOW - DAY),
        writtenAt: iso(NOW - DAY),
      });
      fs.writeFileSync(path.join(dir, `${WITNESS}.json`), JSON.stringify(named));
      // A clean document under a credential-shaped name: refused for the name alone.
      const clean = runDoc('vm.lead', 'clean-1', {
        endedAt: iso(NOW - DAY),
        writtenAt: iso(NOW - DAY),
      });
      fs.writeFileSync(path.join(dir, `vm.lead--${WITNESS}-copy.json`), JSON.stringify(clean));
      writeRun(shareRoot, runDoc('vm.lead', 'ok-1', { endedAt: iso(NOW - DAY) }), day);
      const storeDir = path.join(box, 'store');
      let opened = await openStore(storeDir);
      const runsStore = new RunsStore({
        store: () => opened,
        root: () => shareRoot,
        now: () => NOW,
      });
      const pass = await runsStore.pass({ full: true });
      expect([pass.recorded, pass.refused]).toEqual([1, 2]);
      const refused = await opened.runs<RefusedRow[]>('runsRefused');
      expect(JSON.stringify(refused)).not.toContain(WITNESS);
      // Both names carry it, so both are refused for the name before their text is read.
      expect(refused.map((row) => row.reason)).toEqual([SECRET_NAME_REASON, SECRET_NAME_REASON]);
      // No writer either: only the path's hash, the reason and the time.
      expect(refused.every((row) => !('writer' in row) && row.pathHash.length === 32)).toBe(true);
      // Not in memory for display either: named by writer and hash only.
      const files = (await runsStore.state({})).problems.map((problem) => problem.file);
      expect(JSON.stringify(files)).not.toContain(WITNESS);
      expect(files.filter((file) => file.includes(`${NAME_WITHHELD} `))).toHaveLength(2);
      await opened.close();
      expect(holding(storeDir, WITNESS)).toEqual([]);
      opened = await openStore(storeDir);
      expect(JSON.stringify(await opened.runs('runsRefused'))).not.toContain(WITNESS);
      await opened.close();
      expect(holding(storeDir, WITNESS)).toEqual([]);
      // The share copy does hold it, so the byte check is not vacuous.
      expect(holding(shareRoot, WITNESS).length).toBeGreaterThan(0);
    } finally {
      fs.rmSync(box, { recursive: true, force: true });
    }
  });

  it('H1 recheck: a credential-shaped writer folder is kept nowhere, sealed, reopened or replayed', async () => {
    // Recheck 1's witness: a valid writer name whose name the filter reads as an API key.
    const writer = 'vm.sk-abcdefghijklmnopqrstuvwx';
    const secret = 'sk-abcdefghijklmnopqrstuvwx';
    const box = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-sb-h1w-'));
    try {
      const shareRoot = path.join(box, 'share');
      fs.mkdirSync(path.join(shareRoot, 'v1', 'catalog'), { recursive: true });
      for (const name of fs.readdirSync(CATALOG))
        fs.copyFileSync(path.join(CATALOG, name), path.join(shareRoot, 'v1', 'catalog', name));
      writeRun(shareRoot, runDoc(writer, 'probe'), '2026-11-06');
      fs.writeFileSync(
        path.join(shareRoot, 'v1', 'writers', writer, 'thread.json'),
        JSON.stringify({
          schema: 'agent-reports/thread@1',
          writer,
          seq: 1,
          writtenAt: iso(NOW),
          agent: { t3Thread: 'thread-1' },
        })
      );
      const storeDir = path.join(box, 'store');
      let opened = await openStore(storeDir);
      const runsStore = new RunsStore({
        store: () => opened,
        root: () => shareRoot,
        now: () => NOW,
      });
      const pass = await runsStore.pass({ full: true });
      expect([pass.recorded, pass.refused]).toEqual([0, 1]);
      expect(JSON.stringify(await opened.runs('runsRefused'))).not.toContain(secret);
      const shown = JSON.stringify((await runsStore.state({})).problems);
      expect(shown).not.toContain(secret);
      expect(shown).toContain(NAME_WITHHELD);
      // Sealed (maintenance seals and compresses the ledger), closed, reopened, replayed.
      await opened.maintain({ nowMs: NOW + DAY });
      await opened.close();
      expect(holding(storeDir, secret)).toEqual([]);
      opened = await openStore(storeDir);
      expect(JSON.stringify(await opened.runs('runsRefused'))).not.toContain(secret);
      await opened.close();
      fs.rmSync(path.join(storeDir, 'aac.redb'));
      opened = await openStore(storeDir);
      expect(await opened.runs<RefusedRow[]>('runsRefused')).toHaveLength(1);
      await opened.close();
      expect(holding(storeDir, secret)).toEqual([]);
      expect(holding(shareRoot, secret).length).toBeGreaterThan(0);
    } finally {
      fs.rmSync(box, { recursive: true, force: true });
    }
  });

  it('H2 recheck: URL userinfo in a title and a Basic thread value are stored nowhere', async () => {
    // Recheck 1's witnesses, synthetic only: a password in URL userinfo on a local host, and a
    // short Basic credential as a writer's thread value beside a clean run of that writer.
    const userinfo = 'synthetic-password123';
    const basic = 'c3ludGhldGljOnBhc3N3b3JkMTIz';
    const box = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-sb-h2-'));
    const thread = (shareRoot: string, writer: string, t3Thread: string) =>
      fs.writeFileSync(
        path.join(shareRoot, 'v1', 'writers', writer, 'thread.json'),
        JSON.stringify({
          schema: 'agent-reports/thread@1',
          writer,
          seq: 1,
          writtenAt: iso(NOW),
          agent: { t3Thread },
        })
      );
    try {
      const shareRoot = path.join(box, 'share');
      fs.mkdirSync(path.join(shareRoot, 'v1', 'catalog'), { recursive: true });
      for (const name of fs.readdirSync(CATALOG))
        fs.copyFileSync(path.join(CATALOG, name), path.join(shareRoot, 'v1', 'catalog', name));
      writeRun(
        shareRoot,
        runDoc('vm.synthetic', 'url', {
          task: { id: 'url', type: 'build', title: `https://agent:${userinfo}@localhost/path` },
        })
      );
      writeRun(shareRoot, runDoc('vm.synthetic', 'clean'));
      thread(shareRoot, 'vm.synthetic', `Basic ${basic}`);
      // Control: a plain thread value is still taken, so the thread check is not vacuous.
      writeRun(shareRoot, runDoc('vm.plain', 'clean'));
      thread(shareRoot, 'vm.plain', 'thread-plain-1');
      const storeDir = path.join(box, 'store');
      let opened = await openStore(storeDir);
      const runsStore = new RunsStore({
        store: () => opened,
        root: () => shareRoot,
        now: () => NOW,
      });
      const pass = await runsStore.pass({ full: true });
      expect([pass.recorded, pass.refused]).toEqual([2, 1]);
      const runs = await runsStore.query({});
      const threadOf = (writer: string) =>
        runs.find((run) => run.doc.writer === writer)?.ids.t3ThreadId as
          { value: string } | null | undefined;
      expect(threadOf('vm.synthetic')).toBeNull();
      expect(threadOf('vm.plain')?.value).toBe('thread-plain-1');
      const refused = await opened.runs<RefusedRow[]>('runsRefused');
      for (const needle of [userinfo, basic]) {
        expect(JSON.stringify([runs, refused])).not.toContain(needle);
        expect(JSON.stringify((await runsStore.state({})).problems)).not.toContain(needle);
      }
      // The native module refuses them on its own too: the same texts handed straight to it.
      const read = (relative: string) =>
        fs.readFileSync(path.join(shareRoot, 'v1', relative), 'utf8');
      const urlPath = 'writers/vm.synthetic/runs/2026-11-06/vm.synthetic--url.json';
      const cleanPath = 'writers/vm.synthetic/runs/2026-11-06/vm.synthetic--clean2.json';
      const direct = await opened.runs<{ recorded: number; refused: number }>('runsRecord', {
        files: [
          { path: urlPath, text: read(urlPath), size: 1, mtimeMs: 2 },
          {
            path: cleanPath,
            text: JSON.stringify(runDoc('vm.synthetic', 'clean2')),
            size: 1,
            mtimeMs: 2,
          },
        ],
        refused: [],
        threads: { 'vm.synthetic': read('writers/vm.synthetic/thread.json') },
        catalog: { models: null, aliases: null },
        nowMs: NOW + 1,
      });
      expect([direct.recorded, direct.refused]).toEqual([1, 1]);
      const clean2 = (await runsStore.query({})).find(
        (run) => run.doc.id === 'vm.synthetic--clean2'
      );
      expect(clean2?.ids.t3ThreadId).toBeNull();
      // Sealed (maintenance seals and compresses the ledger), closed, reopened, replayed.
      await opened.maintain({ nowMs: NOW + DAY });
      await opened.close();
      for (const needle of [userinfo, basic]) expect(holding(storeDir, needle)).toEqual([]);
      opened = await openStore(storeDir);
      expect((await opened.runs<{ runs: unknown[] }>('runsQuery', {})).runs).toHaveLength(3);
      await opened.close();
      fs.rmSync(path.join(storeDir, 'aac.redb'));
      opened = await openStore(storeDir);
      expect((await opened.runs<{ runs: unknown[] }>('runsQuery', {})).runs).toHaveLength(3);
      await opened.close();
      for (const needle of [userinfo, basic]) {
        expect(holding(storeDir, needle)).toEqual([]);
        expect(holding(shareRoot, needle).length).toBeGreaterThan(0);
      }
    } finally {
      fs.rmSync(box, { recursive: true, force: true });
    }
  });

  it('M1 and M2: the stored winner is the TypeScript reader winner, over passes and fresh', async () => {
    const box = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-sb-m12-'));
    try {
      const shareRoot = path.join(box, 'share');
      fs.mkdirSync(path.join(shareRoot, 'v1', 'catalog'), { recursive: true });
      for (const name of fs.readdirSync(CATALOG))
        fs.copyFileSync(path.join(CATALOG, name), path.join(shareRoot, 'v1', 'catalog', name));
      const written = iso(NOW - 3 * DAY);
      const put = (local: string, day: string, fields: Record<string, unknown>) =>
        writeRun(
          shareRoot,
          runDoc('vm.lead', local, { endedAt: written, writtenAt: written, ...fields }),
          day
        );
      const opened = await openStore(path.join(box, 'store'));
      const runsStore = new RunsStore({
        store: () => opened,
        root: () => shareRoot,
        now: () => NOW,
      });
      // M2: A (done) on Nov 01, the same bytes on Nov 03, then B (failed) on Nov 02; same seq
      // and writtenAt, so the greatest path decides.
      const a = put('m2', '2026-11-01', { outcome: 'done' });
      await runsStore.pass({ full: true });
      fs.mkdirSync(path.join(shareRoot, 'v1/writers/vm.lead/runs/2026-11-03'), { recursive: true });
      fs.copyFileSync(a, a.replace('2026-11-01', '2026-11-03'));
      await runsStore.pass({ full: true });
      put('m2', '2026-11-02', { outcome: 'failed' });
      // M1: two corrections above 2^32, the higher first.
      put('m1', '2026-11-01', { seq: 2 ** 32 + 1, outcome: 'done' });
      await runsStore.pass({ full: true });
      put('m1', '2026-11-02', { seq: 2 ** 32, outcome: 'failed' });
      await runsStore.pass({ full: true });
      const reader = new RunsReader({ root: () => shareRoot, now: () => NOW });
      await reader.scan();
      const expected = Object.fromEntries(
        reader.snapshot().runs.map((run) => [run.doc.id, [run.file, run.doc.seq, run.doc.outcome]])
      );
      const got = (stored: Awaited<ReturnType<RunsStore['query']>>) =>
        Object.fromEntries(
          stored.map((run) => [run.doc.id, [run.file, run.doc.seq, run.doc.outcome]])
        );
      expect(expected['vm.lead--m2']).toEqual([
        'writers/vm.lead/runs/2026-11-03/vm.lead--m2.json',
        1,
        'done',
      ]);
      expect(expected['vm.lead--m1']).toEqual([
        'writers/vm.lead/runs/2026-11-01/vm.lead--m1.json',
        2 ** 32 + 1,
        'done',
      ]);
      expect(got(await runsStore.query({}))).toEqual(expected);
      // A fresh store reading every file at once agrees too.
      const fresh = await openStore(path.join(box, 'fresh'));
      const freshRuns = new RunsStore({
        store: () => fresh,
        root: () => shareRoot,
        now: () => NOW,
      });
      await freshRuns.pass({ full: true });
      expect(got(await freshRuns.query({}))).toEqual(expected);
      await fresh.close();
      await opened.close();
    } finally {
      fs.rmSync(box, { recursive: true, force: true });
    }
  });
});
