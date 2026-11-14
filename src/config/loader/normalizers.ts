/** Normalizers needed by the CLIProxy configuration held for research. */
import { GO_DURATION_PATTERN, GO_DURATION_SEGMENT } from './io-locks';

// ---------------------------------------------------------------------------
// Session affinity TTL normalizer
// ---------------------------------------------------------------------------

export function normalizeSessionAffinityTtl(value: unknown, fallback: string): string {
  if (typeof value !== 'string') {
    return fallback;
  }

  const trimmed = value.trim();
  if (!trimmed || !GO_DURATION_PATTERN.test(trimmed) || !hasPositiveDuration(trimmed)) {
    return fallback;
  }

  return trimmed;
}

export function hasPositiveDuration(value: string): boolean {
  const segments = value.match(new RegExp(GO_DURATION_SEGMENT, 'g'));
  if (!segments) {
    return false;
  }

  return segments.some((segment) => {
    const numeric = parseFloat(segment);
    return Number.isFinite(numeric) && numeric > 0;
  });
}
