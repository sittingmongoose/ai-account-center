/** The Scoreboard's arithmetic: medians with their range and 90% Wilson intervals. Pure. */

/** The z value of a two-sided 90% interval. */
const Z_90 = 1.6448536269514722;

export interface Interval {
  low: number;
  high: number;
}

/** 90% Wilson interval of `of` out of `per`; null without a denominator or for an impossible share. */
export function wilson90(of: number, per: number): Interval | null {
  if (!(per > 0) || of < 0 || of > per) return null;
  const p = of / per;
  const z2 = Z_90 * Z_90;
  const denominator = 1 + z2 / per;
  const centre = (p + z2 / (2 * per)) / denominator;
  const margin = (Z_90 * Math.sqrt((p * (1 - p)) / per + z2 / (4 * per * per))) / denominator;
  return { low: Math.max(0, centre - margin), high: Math.min(1, centre + margin) };
}

/** The middle value (the mean of the two middle values for an even count); null for none. */
export function median(values: readonly number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

/** Rounds to `digits` significant digits, for numbers the page prints. */
export function significant(value: number, digits = 4): number {
  if (value === 0 || !Number.isFinite(value)) return value;
  return Number(value.toPrecision(digits));
}

export function roundOrNull(value: number | null | undefined): number | null {
  return value === null || value === undefined ? null : significant(value);
}
