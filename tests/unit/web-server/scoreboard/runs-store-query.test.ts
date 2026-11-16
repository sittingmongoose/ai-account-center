/**
 * One snapshot per paged read (run-corrections recheck L1): every runsQuery page names its
 * generation and catalog hash, and RunsStore.query restarts from the start when a commit
 * lands between pages, so one view never mixes two snapshots. The scripted handle serves
 * the recheck's witness (b0..b3 with b3-fix, then b0-fix ingested between pages) through
 * the real AacStore/RunsStore path; no NAPI module needed.
 */
import { describe, expect, it } from 'bun:test';
import { AacStore } from '../../../../src/store/store-service';
import type { NativeStoreHandle } from '../../../../src/store/native-loader';
import { RunsStore, type StoredRun } from '../../../../src/web-server/scoreboard/runs-store';
import { runDoc } from './scoreboard-test-helpers';

const CATALOG = 'c'.repeat(64);
const MOVED_CATALOG = 'd'.repeat(64);

interface Leaf {
  id: string;
  endedMs: number;
}

const row = (leaf: Leaf) => ({
  file: `writers/vm.probe/runs/${leaf.id}.json`,
  sha256: leaf.id,
  firstSeenAt: 1,
  doc: runDoc('vm.probe', leaf.id),
  identity: {},
  ids: {},
});

/**
 * The store's two snapshots as the recheck reproduced them: generation 7 keeps b0 until
 * b0-fix lands; generation 8 resolves b0 to b0-fix (endedMs 5) and drops b0 itself.
 * A pending ingest or catalog move commits when the next later page is fetched, exactly
 * between two pages of one read.
 */
function scriptedStore() {
  const snapshots: Record<number, Leaf[]> = {
    7: [
      { id: 'b0', endedMs: 0 },
      { id: 'b1', endedMs: 1 },
      { id: 'b2', endedMs: 2 },
      { id: 'b3-fix', endedMs: 3 },
    ],
    8: [
      { id: 'b1', endedMs: 1 },
      { id: 'b2', endedMs: 2 },
      { id: 'b3-fix', endedMs: 3 },
      { id: 'b0-fix', endedMs: 5 },
    ],
  };
  let generation = 7;
  let catalogHash = CATALOG;
  let ingestPending = false;
  let catalogPending = false;
  const calls: Array<{ after: unknown; generation: number; catalogHash: string }> = [];
  const handle: NativeStoreHandle = {
    async call(method, args) {
      if (method === 'runsRefused') return '[]';
      if (method !== 'runsQuery') throw new Error(`unexpected method ${method}`);
      const { after } = JSON.parse(args) as { after: [number, number] | null };
      if (after !== null) {
        if (ingestPending) {
          generation = 8;
          ingestPending = false;
        }
        if (catalogPending) {
          catalogHash = MOVED_CATALOG;
          catalogPending = false;
        }
      }
      calls.push({ after, generation, catalogHash });
      const leaves = snapshots[generation];
      const start = after === null ? 0 : leaves.findIndex((leaf) => leaf.endedMs > after[0]);
      const slice = leaves.slice(start, start + 2);
      const last = slice[slice.length - 1];
      const next = start + 2 < leaves.length && last ? [last.endedMs, 0] : null;
      const page: Record<string, unknown> = {
        runs: slice.map(row),
        next,
        skipped: 0,
        generation,
        catalogHash,
      };
      if (after === null) {
        page.problems = [{ file: `gen-${generation}-${catalogHash[0]}`, reason: 'fixture', since: 1 }];
        page.accepted = [row(leaves[0])];
      }
      return JSON.stringify(page);
    },
  };
  return {
    handle,
    calls,
    ingestBetweenPages: () => {
      ingestPending = true;
    },
    moveCatalogBetweenPages: () => {
      catalogPending = true;
    },
  };
}

function runsStore(handle: NativeStoreHandle): RunsStore {
  const store = new AacStore(handle, '/fixture/store');
  return new RunsStore({ store: () => store, root: () => '/fixture/share', now: () => 1 });
}

const ids = (runs: StoredRun[]) => runs.map((run) => run.doc.id);

describe('RunsStore.query serves one snapshot per read', () => {
  it('pages of one generation concatenate without a restart', async () => {
    const script = scriptedStore();
    const runs = await runsStore(script.handle).query({});
    expect(ids(runs)).toEqual([
      'vm.probe--b0',
      'vm.probe--b1',
      'vm.probe--b2',
      'vm.probe--b3-fix',
    ]);
    expect(script.calls.map((call) => call.after === null)).toEqual([true, false]);
  });

  it('restarts from the start when an ingest lands between pages', async () => {
    const script = scriptedStore();
    const store = runsStore(script.handle);
    await store.query({});
    script.calls.length = 0;
    script.ingestBetweenPages();
    const runs = await store.query({});
    // The mixed read would count b0 twice (Sol and Opus); the restart serves generation 8.
    expect(ids(runs)).toEqual([
      'vm.probe--b1',
      'vm.probe--b2',
      'vm.probe--b3-fix',
      'vm.probe--b0-fix',
    ]);
    // First page (gen 7), the stale-cursor page (gen 8), then the restart's two pages.
    expect(script.calls.map((call) => call.after === null)).toEqual([true, false, true, false]);
    expect(script.calls.map((call) => call.generation)).toEqual([7, 8, 8, 8]);
  });

  it('restarts on a catalog-only change and refreshes the first-page facts', async () => {
    const script = scriptedStore();
    const store = runsStore(script.handle);
    await store.query({});
    script.calls.length = 0;
    script.moveCatalogBetweenPages();
    const runs = await store.query({});
    // Same rows, new key: the read still restarts, so its facts come from one snapshot.
    expect(ids(runs)).toEqual([
      'vm.probe--b0',
      'vm.probe--b1',
      'vm.probe--b2',
      'vm.probe--b3-fix',
    ]);
    expect(script.calls.map((call) => call.after === null)).toEqual([true, false, true, false]);
    expect(script.calls.map((call) => call.catalogHash)).toEqual([
      CATALOG,
      MOVED_CATALOG,
      MOVED_CATALOG,
      MOVED_CATALOG,
    ]);
    const state = await store.state({});
    expect(state.problems.some((problem) => problem.file === 'gen-7-d')).toBe(true);
    expect(state.problems.some((problem) => problem.file === 'gen-7-c')).toBe(false);
  });
});
