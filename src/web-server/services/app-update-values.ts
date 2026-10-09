/** Bounded value checks shared by the Update apps contract and its row parts. */

export function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export function safeVersion(value: unknown): string | null {
  return typeof value === 'string' &&
    value.length <= 64 &&
    /^\d+(?:\.[0-9A-Za-z-]+){1,5}(?:[+-][0-9A-Za-z.-]+)?$/.test(value)
    ? value
    : null;
}
