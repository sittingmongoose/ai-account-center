const MIN_INTERVAL_SECONDS = 120;
const MAX_DELAY_SECONDS = 86400;
const MAX_BACKOFF_SECONDS = 1800;
const JITTER_MS = 300_000;

// Five doublings reach the ceiling (120, 240, 480, 960, 1800).
export const MAX_CLAUDE_429_STREAK = 5;

/** The target digest gives each profile a stable offset across dashboard views and restarts. */
export function claudeDesktopDelayMs(
  targetDigest: string,
  floorSeconds: number,
  rateLimitStreak = 0
): number {
  const jitterMs = Number.parseInt(targetDigest.slice(0, 8), 16) % JITTER_MS;
  const baseSeconds = rateLimitStreak
    ? Math.min(MAX_BACKOFF_SECONDS, MIN_INTERVAL_SECONDS * 2 ** (rateLimitStreak - 1))
    : MIN_INTERVAL_SECONDS;
  return Math.min(MAX_DELAY_SECONDS * 1000, Math.max(baseSeconds, floorSeconds) * 1000 + jitterMs);
}
