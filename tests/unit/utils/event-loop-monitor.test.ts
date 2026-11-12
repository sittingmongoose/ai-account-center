import { describe, expect, it, spyOn } from 'bun:test';
import { performance } from 'perf_hooks';
import { startEventLoopMonitor } from '../../../src/utils/event-loop-monitor';

describe('dashboard event-loop observation', () => {
  it('measures drift, tolerates a backwards clock and starts a fresh window after a log failure', () => {
    let now = 100;
    let tick!: () => void;
    let unref = false;
    let cleared = false;
    const clock = spyOn(performance, 'now').mockImplementation(() => now);
    const interval = spyOn(globalThis, 'setInterval').mockImplementation((callback) => {
      tick = callback as () => void;
      return { unref: () => (unref = true) } as unknown as ReturnType<typeof setInterval>;
    });
    const clear = spyOn(globalThis, 'clearInterval').mockImplementation(() => {
      cleared = true;
    });
    const entries: Record<string, unknown>[] = [];
    let stop: (() => void) | undefined;
    try {
      stop = startEventLoopMonitor(
        {
          info: (_event, _message, context) => {
            entries.push(context!);
            if (entries.length === 1) throw new Error('Fixture log disk unavailable');
          },
        },
        100
      );
      expect(unref).toBe(true);
      for (now of [110, 90, 100, 150, 200]) tick();
      expect(entries).toHaveLength(1);
      expect(entries[0]).toMatchObject({ maxMs: 40, p99Ms: 40, meanMs: 16 });
      for (now of [210, 220, 230, 240, 250, 260, 270, 280, 290, 300]) tick();
      expect(entries).toHaveLength(2);
      expect(entries[1]).toMatchObject({ maxMs: 0, p99Ms: 0, meanMs: 0 });
      stop();
      expect(cleared).toBe(true);
    } finally {
      stop?.();
      clear.mockRestore();
      interval.mockRestore();
      clock.mockRestore();
    }
  });

  it('logs bounded numeric counters at info level and stops with its server', async () => {
    const entries: { event: string; context: Record<string, unknown> | undefined }[] = [];
    const stop = startEventLoopMonitor(
      { info: (event, _message, context) => entries.push({ event, context }) },
      40
    );
    try {
      await new Promise((resolve) => setTimeout(resolve, 110));
      expect(entries.length).toBeGreaterThan(0);
      expect(entries[0].event).toBe('server.event_loop_delay');
      expect(Object.keys(entries[0].context!).sort()).toEqual([
        'externalBytes',
        'heapUsedBytes',
        'maxMs',
        'meanMs',
        'p99Ms',
        'rssBytes',
      ]);
      expect(
        Object.values(entries[0].context!).every(
          (value) => typeof value === 'number' && Number.isFinite(value)
        )
      ).toBe(true);
    } finally {
      stop();
    }
    const count = entries.length;
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(entries).toHaveLength(count);
  });
});
