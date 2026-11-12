import { performance } from 'perf_hooks';
import type { Logger } from '../services/logging/logger';

/** One unref'd JS timer; at most 48 KB of lag samples, never request data. */
export function startEventLoopMonitor(
  logger: Pick<Logger, 'info'>,
  intervalMs = 60_000
): () => void {
  const resolutionMs = Math.min(10, Math.max(1, intervalMs));
  const samples = new Float64Array(6000);
  let count = 0;
  let cursor = 0;
  let total = 0;
  let maximum = 0;
  let expected = performance.now() + resolutionMs;
  let reportAt = performance.now() + intervalMs;
  const timer = setInterval(() => {
    const now = performance.now();
    const lag = Math.max(0, now - expected);
    // Rebase after a stall: one late tick must not create repeated phantom stalls.
    expected = now + resolutionMs;
    samples[cursor] = lag;
    cursor = (cursor + 1) % samples.length;
    count++;
    total += lag;
    maximum = Math.max(maximum, lag);
    if (now < reportAt) return;

    const sorted = samples.slice(0, Math.min(count, samples.length)).sort();
    const memory = process.memoryUsage();
    const ms = (value: number) => Math.round(value * 100) / 100;
    const context = {
      p99Ms: ms(sorted[Math.max(0, Math.ceil(sorted.length * 0.99) - 1)]),
      maxMs: ms(maximum),
      meanMs: ms(total / count),
      rssBytes: memory.rss,
      heapUsedBytes: memory.heapUsed,
      externalBytes: memory.external,
    };
    count = 0;
    cursor = 0;
    total = 0;
    maximum = 0;
    reportAt = now + intervalMs;
    try {
      logger.info('server.event_loop_delay', 'Dashboard event-loop delay', context);
    } catch {
      // A failed log disk must not stop the dashboard or the next observation.
    }
  }, resolutionMs);
  timer.unref();
  return () => clearInterval(timer);
}
