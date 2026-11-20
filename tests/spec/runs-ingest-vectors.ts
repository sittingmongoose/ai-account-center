/**
 * G16b: the Scoreboard's run ingest, TS vs Rust, on the same fixture runs. Each pass runs the real
 * TS `RunsStore.pass` over a temporary share and a temporary store (the real aac-store module) and
 * records what the store's `runsScan` returned and the exact `runsRecord` request the TS ingest
 * built from it (the secret filter over every path, writer folder and text, the refusals and their
 * reasons), with the pass problems. `crates/aac-http` (scoreboard_owned `ingest_request`, the
 * service's owned ingest) must build the same request and problems from the same scan.
 *
 * Needs the store module: `AAC_STORE_MODULE=<aac_store .node>` or dist/native. Never the real share
 * or home; the clock is fixed, so the file is the same on every run.
 *
 *   bun tests/spec/runs-ingest-vectors.ts          # check tests/spec/runs-ingest-vectors.json
 *   bun tests/spec/runs-ingest-vectors.ts --write  # regenerate it (review the diff)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadNativeStore, nativeModuleName } from '../../src/store/native-loader';
import { AacStore } from '../../src/store/store-service';
import { RunsStore } from '../../src/web-server/scoreboard/runs-store';
import { NOW } from '../unit/web-server/status/status-test-helpers';
import { runDoc, writeRun } from '../unit/web-server/scoreboard/scoreboard-test-helpers';

const MODULE =
  process.env.AAC_STORE_MODULE ??
  path.resolve(import.meta.dir, '../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });
if (!loaded.module) throw new Error(`the store module is needed: ${MODULE} (${loaded.reason})`);

const CATALOG = path.resolve(import.meta.dir, '../../docs/agent-reports/catalog');
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const iso = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z');
const WITNESS = 'sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123';

const box = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-runs-ingest-'));
const root = path.join(box, 'share');
fs.mkdirSync(path.join(root, 'v1', 'catalog'), { recursive: true });
for (const name of fs.readdirSync(CATALOG).sort())
  fs.copyFileSync(path.join(CATALOG, name), path.join(root, 'v1', 'catalog', name));

/** Files are named and timed by the fixture, not the run: mtimes fixed so scans are repeatable. */
function stamp(file: string, ms: number) {
  fs.utimesSync(file, new Date(ms), new Date(ms));
}
function put(writer: string, local: string, ended: number, fields: Record<string, unknown> = {}) {
  const doc = runDoc(writer, local, { endedAt: iso(ended), writtenAt: iso(ended), ...fields });
  stamp(writeRun(root, doc, iso(ended).slice(0, 10)), ended);
  return doc;
}
function raw(writer: string, name: string, ended: number, text: string) {
  const dir = path.join(root, 'v1', 'writers', writer, 'runs', iso(ended).slice(0, 10));
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, name);
  fs.writeFileSync(file, text);
  stamp(file, ended);
}
function thread(writer: string, text: string) {
  const file = path.join(root, 'v1', 'writers', writer, 'thread.json');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
  stamp(file, NOW - HOUR);
}

// The first batch: kinds the ingest must keep, and every refusal it makes.
put('vm.lead', 'b0', NOW - 9 * DAY, {
  model: { provider: 'party', id: 'gpt-6.1-sol', harness: 'claude-code', effort: 'X-High' },
});
put('vm.lead', 'rv0', NOW - 9 * DAY + 2 * HOUR, {
  task: { id: 'rv0', type: 'review', title: 'Review 0' },
  model: { provider: 'anthropic', id: 'claude-opus-5-5', harness: 't3-delegate', effort: 'high' },
  review: {
    of: 'vm.lead--b0',
    verdict: 'pass',
    findings: { high: 0, medium: 0, low: 0 },
    overturned: false,
  },
});
put('mac.worker', 'qwen', NOW - 3 * DAY, {
  model: { provider: 'omp', id: 'alibaba-token-plan/qwen3.8-max', harness: 'omp', effort: null },
});
put('vm.lead', 'bench', NOW - 2 * DAY, { source: 'benchmark', benchmark: 'pilot' });
put('vm.lead', 'secret', NOW - DAY, {
  task: { id: 'secret', type: 'build', title: `uses ${WITNESS}` },
});
put('vm.lead', 'mail', NOW - DAY, {
  task: { id: 'mail', type: 'build', title: 'asks someone@example.com' },
});
raw(
  'vm.lead',
  `vm.lead--${WITNESS}-copy.json`,
  NOW - DAY,
  JSON.stringify(runDoc('vm.lead', 'clean-1', { endedAt: iso(NOW - DAY) }))
);
raw('vm.lead', 'vm.lead--torn.json', NOW - DAY, '{"schema":"agent-reports/run@1","id":');
raw(
  'vm.lead',
  'vm.lead--big.json',
  NOW - DAY,
  JSON.stringify(runDoc('vm.lead', 'big', { notes: 'x'.repeat(40 * 1024) }))
);
thread('vm.lead', JSON.stringify({ schema: 'agent-reports/thread@1', writer: 'vm.lead', seq: 1 }));
thread('mac.worker', JSON.stringify({ note: `key ${WITNESS}` }));
put('vm.sk-abcdefghijklmnopqrstuvwx', 'w1', NOW - DAY);

const handle = new loaded.module.NativeStore();
await handle.call('open', JSON.stringify({ dir: path.join(box, 'store') }));
const store = new AacStore(handle, path.join(box, 'store'));

const capture: { scan?: unknown; record?: unknown } = {};
const realRuns = store.runs.bind(store);
(store as { runs: typeof store.runs }).runs = (async (method: string, args?: unknown) => {
  const out = await realRuns(method as Parameters<typeof store.runs>[0], args);
  if (method === 'runsScan') capture.scan = out;
  if (method === 'runsRecord') capture.record = args;
  return out;
}) as typeof store.runs;

const clock = { now: NOW };
const runs = new RunsStore({ store: () => store, root: () => root, now: () => clock.now });
const passes: unknown[] = [];
async function pass(name: string, full: boolean) {
  capture.scan = capture.record = undefined;
  const state = await runs.pass({ full });
  passes.push(
    JSON.parse(
      JSON.stringify({
        name,
        now: clock.now,
        full,
        scan: capture.scan,
        record: capture.record,
        problems: state.problems,
        more: state.more,
        recorded: state.recorded,
        refused: state.refused,
      })
    )
  );
}

await pass('first full pass', true);
// Again at once: everything known, nothing recorded.
clock.now = NOW + 60_000;
await pass('repeat', true);
// New files, an incremental pass.
put('vm.lead', 'late', NOW - HOUR);
raw('vm.lead', 'vm.lead--late-torn.json', NOW - HOUR, 'not json');
clock.now = NOW + 120_000;
await pass('incremental', false);

const stored = await store.runs<Array<{ doc: { id: string } }>>('runsQuery', {});
await store.close();
fs.rmSync(box, { recursive: true, force: true });

// The fixture's random temp root appears in the scan only as `root`-relative paths; check that.
const text = JSON.stringify(passes);
assert(!text.includes(box), 'a scan or request names the temporary root');
const vectors = { passes, storedIds: (stored as { runs?: unknown[] }).runs?.length ?? null };
const file = path.join(import.meta.dir, 'runs-ingest-vectors.json');
if (process.argv.includes('--write')) {
  fs.writeFileSync(file, JSON.stringify(vectors, null, 1) + '\n');
} else {
  assert.deepEqual(
    JSON.parse(fs.readFileSync(file, 'utf8')),
    vectors,
    'runs-ingest-vectors.json is stale; regenerate and review'
  );
}
console.log(`runs ingest vectors: ${passes.length} passes`);
