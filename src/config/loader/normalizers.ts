/** Normalizers needed by the CLIProxy configuration held for research. */
import type { UnifiedConfig } from '../unified-config-types';
import { validateCompositeTiers } from '../../cliproxy/config/composite-validator';
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

// ---------------------------------------------------------------------------
// Composite variant validator
// ---------------------------------------------------------------------------

/**
 * Validate composite variant provider strings.
 * Warns about invalid providers in composite variant configurations.
 */
export function validateCompositeVariants(config: UnifiedConfig): void {
  const variants = config.cliproxy?.variants;
  if (!variants) return;

  for (const [name, variant] of Object.entries(variants)) {
    if ('type' in variant && variant.type === 'composite') {
      const error = validateCompositeTiers(variant.tiers, {
        defaultTier: variant.default_tier,
        requireAllTiers: true,
      });
      if (error) {
        console.warn(`[!] Variant '${name}': invalid composite config (${error})`);
      }
    }
  }
}
