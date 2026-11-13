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
import { RunsStore } from '../../../../src/web-server/scoreboard/runs-store';
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
    const refused = await store!.runs<Array<{ file: string; reason: string }>>('runsRefused');
    expect(refused).toEqual([
      expect.objectContaining({
        file: expect.stringContaining('vm.lead--secret.json'),
        reason: 'refused: it contains what looks like an API key',
      }),
    ]);
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
