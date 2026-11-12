/** Estimate retained DTO storage without allocating another whole JSON body.
 * Shared objects/arrays are charged once; slots, UTF-16 strings and object overhead
 * are included. This is a budget estimate, not a V8 heap-size measurement.
 */
function retainedWeight(value: unknown, limit: number): number {
  const seen = new WeakSet<object>();
  const keys = new Set<string>();
  let bytes = 0;
  const visit = (item: unknown, depth: number): void => {
    if (bytes > limit) return;
    if (typeof item === 'string') {
      bytes += 32 + item.length * 2;
      return;
    }
    if (item === null || typeof item !== 'object') {
      bytes += 8;
      return;
    }
    if (seen.has(item)) return;
    seen.add(item);
    if (depth > 64) {
      bytes = limit + 1;
      return;
    }
    bytes += 32;
    if (ArrayBuffer.isView(item)) {
      bytes += item.buffer.byteLength;
      return;
    }
    if (item instanceof ArrayBuffer) {
      bytes += item.byteLength;
      return;
    }
    if (Array.isArray(item)) {
      bytes += item.length * 8;
      for (const child of item) {
        visit(child, depth + 1);
        if (bytes > limit) break;
      }
    } else {
      for (const key of Object.keys(item)) {
        bytes += 16;
        if (!keys.has(key)) {
          keys.add(key);
          bytes += 32 + key.length * 2;
        }
        visit((item as Record<string, unknown>)[key], depth + 1);
        if (bytes > limit) break;
      }
    }
  };
  visit(value, 0);
  return bytes;
}

/** A small LRU with an estimated retained-byte budget, including its keys.
 * Oversized answers are still returned to the caller; they are never retained here.
 */
export class BoundedCache<T> {
  private readonly entries = new Map<string, { value: T; weight: number }>();
  private retainedBytes = 0;

  constructor(
    readonly maxEntries: number,
    readonly maxBytes: number
  ) {}

  get size(): number {
    return this.entries.size;
  }

  get bytes(): number {
    return this.retainedBytes;
  }

  get(key: string): T | undefined {
    const entry = this.entries.get(key);
    if (!entry) return undefined;
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.value;
  }

  set(key: string, value: T): void {
    const old = this.entries.get(key);
    if (old) {
      this.retainedBytes -= old.weight;
      this.entries.delete(key);
    }
    const overhead = 2 * key.length + 256;
    const weight = retainedWeight(value, this.maxBytes - overhead) + overhead;
    if (weight > this.maxBytes || this.maxEntries < 1) return;
    this.entries.set(key, { value, weight });
    this.retainedBytes += weight;
    while (this.entries.size > this.maxEntries || this.retainedBytes > this.maxBytes) {
      const oldest = this.entries.entries().next().value;
      if (!oldest) break;
      this.entries.delete(oldest[0]);
      this.retainedBytes -= oldest[1].weight;
    }
  }

  clear(): void {
    this.entries.clear();
    this.retainedBytes = 0;
  }
}
