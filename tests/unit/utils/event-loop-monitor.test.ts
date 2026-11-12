import { describe, expect, it } from 'bun:test';
import { startEventLoopMonitor } from '../../../src/utils/event-loop-monitor';

describe('dashboard event-loop observation', () => {
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
