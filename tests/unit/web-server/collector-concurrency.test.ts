import { describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import * as path from 'path';
import {
  COLLECTOR_NICE,
  MAX_CONCURRENT_COLLECTORS,
  collectorConcurrency,
  lowerCollectorThreadPriority,
  runBounded,
} from '../../../src/web-server/usage/collector-concurrency';

const GiB = 1024 ** 3;
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A task whose finish the test controls, recording how many run at once. */
function controlled() {
  const releases = new Map<number, () => void>();
  const started: number[] = [];
  let live = 0;
  let maximum = 0;
  const task = (_item: string, index: number) => {
    started.push(index);
    live++;
    maximum = Math.max(maximum, live);
    return new Promise<void>((resolve) => {
      releases.set(index, () => {
        live--;
        resolve();
      });
    });
  };
  return { task, releases, started, maximum: () => maximum };
}

describe('collector concurrency', () => {
  it('sizes the bound to the machine: a quarter of the CPUs, what memory holds, between 2 and the cap', () => {
    // This VM: 56 cores, 30 GB.
    expect(collectorConcurrency(56, 30 * GiB)).toBe(MAX_CONCURRENT_COLLECTORS);
    expect(MAX_CONCURRENT_COLLECTORS).toBe(8);
    expect(collectorConcurrency(16, 64 * GiB)).toBe(4);
    // Memory bounds it before CPUs do.
    expect(collectorConcurrency(32, 3 * GiB)).toBe(3);
    // A small laptop keeps the old pair, never fewer.
    expect(collectorConcurrency(4, 8 * GiB)).toBe(2);
    expect(collectorConcurrency(1, 0.5 * GiB)).toBe(2);
    expect(collectorConcurrency()).toBeGreaterThanOrEqual(2);
    expect(collectorConcurrency()).toBeLessThanOrEqual(MAX_CONCURRENT_COLLECTORS);
  });

  it('starts every item at once when the bound allows', async () => {
    const run = controlled();
    const done = runBounded([['a', 'b', 'c', 'd', 'e']], 8, run.task);
    await tick();
    expect(run.started).toEqual([0, 1, 2, 3, 4]);
    for (const release of run.releases.values()) release();
    expect((await done).items).toEqual(['a', 'b', 'c', 'd', 'e']);
  });

  it('never runs more than the bound, and a slow item never holds back the rest', async () => {
    const run = controlled();
    const done = runBounded([['a', 'b', 'c', 'd', 'e']], 2, run.task);
    await tick();
    expect(run.started).toEqual([0, 1]);
    // Item 0 stays slow; each finish of another item starts the next one at once.
    run.releases.get(1)?.();
    await tick();
    expect(run.started).toEqual([0, 1, 2]);
    run.releases.get(2)?.();
    await tick();
    expect(run.started).toEqual([0, 1, 2, 3]);
    run.releases.get(3)?.();
    await tick();
    expect(run.started).toEqual([0, 1, 2, 3, 4]);
    run.releases.get(4)?.();
    run.releases.get(0)?.();
    const result = await done;
    expect(result.skipped).toBe(0);
    expect(run.maximum()).toBe(2);
  });

  it('adds a later batch when it resolves without waiting for it to start the first', async () => {
    const run = controlled();
    let resolveLater: (items: string[]) => void = () => {};
    const later = new Promise<string[]>((resolve) => {
      resolveLater = resolve;
    });
    const done = runBounded([['ready'], later], 4, run.task);
    await tick();
    expect(run.started).toEqual([0]);
    resolveLater(['found-1', 'found-2']);
    await tick();
    expect(run.started).toEqual([0, 1, 2]);
    for (const release of run.releases.values()) release();
    expect((await done).items).toEqual(['ready', 'found-1', 'found-2']);
  });

  it('survives a rejected batch, a throwing task and a stop, and honours maxItems', async () => {
    let stop = false;
    const seen: string[] = [];
    const result = await runBounded(
      [['a', 'b', 'c', 'd'], Promise.reject(new Error('discovery failed'))],
      1,
      async (item) => {
        seen.push(item);
        if (item === 'a') throw new Error('one collector failed');
        if (item === 'b') stop = true;
      },
      { stop: () => stop, maxItems: 3 }
    );
    expect(seen).toEqual(['a', 'b']);
    expect(result.items).toEqual(['a', 'b', 'c']);
    expect(result.skipped).toBe(1);
    expect(await runBounded([], 2, async () => {})).toEqual({ items: [], skipped: 0 });
  });

  it('lowers only the calling thread on Linux and nothing elsewhere', () => {
    expect(lowerCollectorThreadPriority('darwin')).toBe(false);
    expect(lowerCollectorThreadPriority('win32')).toBe(false);
    if (process.platform !== 'linux') return;
    // In a separate process: the main thread lowers itself; every other thread keeps its nice.
    const modulePath = path.resolve(
      __dirname,
      '../../../src/web-server/usage/collector-concurrency.ts'
    );
    const script = `
      const fs = require('fs');
      const os = require('os');
      const { lowerCollectorThreadPriority } = require(${JSON.stringify(modulePath)});
      const nice = (tid) => os.getPriority(tid);
      const others = () => fs.readdirSync('/proc/self/task').map(Number).filter((t) => t !== process.pid);
      const before = Object.fromEntries(others().map((t) => [t, nice(t)]));
      const applied = lowerCollectorThreadPriority();
      const after = Object.fromEntries(Object.keys(before).map((t) => [t, nice(Number(t))]));
      console.log(JSON.stringify({ applied, main: nice(process.pid), before, after }));
    `;
    const child = spawnSync(process.execPath, ['-e', script], { encoding: 'utf8' });
    expect(child.status).toBe(0);
    const report = JSON.parse(child.stdout.trim().split('\n').pop() ?? '{}') as {
      applied: boolean;
      main: number;
      before: Record<string, number>;
      after: Record<string, number>;
    };
    expect(report.applied).toBe(true);
    expect(report.main).toBeGreaterThanOrEqual(COLLECTOR_NICE);
    for (const [tid, value] of Object.entries(report.before)) {
      if (tid in report.after) expect(report.after[tid]).toBe(value);
    }
  });
});
