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

/**
 * The review's starvation schedule (M1): 5,001 rows, so the 5,000-row first page has a
 * next cursor; every second page commits again (generation or catalog), so a restarting
 * read never settles. The sentinel throws long before any unbounded loop could finish.
 */
function churningStore(catalogOnly = false) {
  const rows = Array.from({ length: 5001 }, (_, i) => row({ id: `b${i}`, endedMs: i }));
  let generation = 1;
  let catalogHash = CATALOG;
  let churn = false;
  let calls = 0;
  let firstPages = 0;
  const handle: NativeStoreHandle = {
    async call(method, args) {
      if (method === 'runsRefused') return '[]';
      if (method !== 'runsQuery') throw new Error(`unexpected method ${method}`);
      calls += 1;
      if (calls > 40) throw new Error('churn sentinel: the query never settled');
      const { after } = JSON.parse(args) as { after: [number, number] | null };
      if (after === null) firstPages += 1;
      else if (churn) {
        if (catalogOnly) catalogHash = catalogHash === CATALOG ? MOVED_CATALOG : CATALOG;
        else generation += 1;
      }
      const page =
        after === null
          ? { runs: rows.slice(0, 5000), next: [4999, 4999] }
          : { runs: rows.slice(5000), next: null };
      return JSON.stringify({
        ...page,
        skipped: 0,
        generation,
        catalogHash,
        ...(after === null ? { problems: [], accepted: [] } : {}),
      });
    },
  };
  return {
    handle,
    startChurn: () => {
      churn = true;
    },
    stats: () => ({ calls, firstPages, generation }),
  };
}

const storeWith = (handle: NativeStoreHandle, now: () => number) =>
  new RunsStore({
    store: () => new AacStore(handle, '/fixture/store'),
    root: () => '/fixture/share',
    now,
  });

describe('a paged read terminates while imports continue (review M1)', () => {
  it('fails explicitly when the store never settles and no complete read exists', async () => {
    const script = churningStore();
    script.startChurn();
    const store = storeWith(script.handle, () => 1);
    await expect(store.query({})).rejects.toThrow(/store_moving/);
    // Four attempts of two pages at most, never the sentinel.
    expect(script.stats().calls).toBeLessThanOrEqual(10);
  });

  it('catalog-only churn also terminates with the explicit failure', async () => {
    const script = churningStore(true);
    script.startChurn();
    const store = storeWith(script.handle, () => 1);
    await expect(store.query({})).rejects.toThrow(/store_moving/);
    expect(script.stats().calls).toBeLessThanOrEqual(10);
  });

  it('serves the last complete read, stamped at that read, when the store never settles', async () => {
    const script = churningStore();
    let now = 1000;
    const store = storeWith(script.handle, () => now);
    const first = await store.query({});
    expect(first.length).toBe(5001);
    script.startChurn();
    now = 5000;
    const runs = await store.query({});
    // The retained read is the complete earlier snapshot, never a mixed accumulation.
    expect(runs.length).toBe(5001);
    expect(runs[0].doc.id).toBe('vm.probe--b0');
    expect(runs[5000].doc.id).toBe('vm.probe--b5000');
    expect(script.stats().calls).toBeLessThanOrEqual(20);
    const state = await store.state({});
    // The answer says it was read up to the retained snapshot, not the later clock.
    expect(state.scannedAt).toBe(1000);
    expect(state.runs.length).toBe(5001);
    expect(
      state.problems.some(
        (problem) => problem.file === 'store/read' && /generation 1/.test(problem.reason)
      )
    ).toBe(true);
  });

  it('stamps a completed read at its snapshot, not at a later clock', async () => {
    const script = scriptedStore();
    let now = 1000;
    let seen = 0;
    const inner = script.handle.call.bind(script.handle);
    const handle: NativeStoreHandle = {
      async call(method, args) {
        if (method === 'runsQuery') {
          seen += 1;
          if (seen === 2) now = 5000;
        }
        return inner(method, args);
      },
    };
    const state = await storeWith(handle, () => now).state({});
    expect(state.scannedAt).toBe(1000);
  });
});
