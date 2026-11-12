import { monitorEventLoopDelay } from 'perf_hooks';
import type { Logger } from '../services/logging/logger';

/** One interval per dashboard; milliseconds and memory counters only, never request data. */
export function startEventLoopMonitor(
  logger: Pick<Logger, 'info'>,
  intervalMs = 60_000
): () => void {
  const histogram = monitorEventLoopDelay({ resolution: 10 });
  histogram.enable();
  const timer = setInterval(() => {
    const memory = process.memoryUsage();
    const ms = (value: number) => (Number.isFinite(value) ? Math.round(value / 10_000) / 100 : 0);
    try {
      logger.info('server.event_loop_delay', 'Dashboard event-loop delay', {
        p99Ms: ms(histogram.percentile(99)),
        maxMs: ms(histogram.max),
        meanMs: ms(histogram.mean),
        rssBytes: memory.rss,
        heapUsedBytes: memory.heapUsed,
        externalBytes: memory.external,
      });
    } catch {
      // A failed log disk must not stop the dashboard or the next observation.
    } finally {
      histogram.reset();
    }
  }, intervalMs);
  timer.unref();
  return () => {
    clearInterval(timer);
    histogram.disable();
  };
}
