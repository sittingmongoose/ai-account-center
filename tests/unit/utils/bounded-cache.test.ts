import { describe, expect, it } from 'bun:test';
import { BoundedCache } from '../../../src/utils/bounded-cache';

describe('bounded retained answers', () => {
  it('evicts by bytes and recent use, and never retains an oversized result', () => {
    const cache = new BoundedCache<{ data: string }>(2, 1500);
    cache.set('a', { data: 'a'.repeat(50) });
    cache.set('b', { data: 'b'.repeat(50) });
    expect(cache.get('a')?.data).toHaveLength(50);
    cache.set('c', { data: 'c'.repeat(50) });
    expect(cache.get('b')).toBeUndefined();
    expect(cache.get('a')).toBeDefined();
    expect(cache.bytes).toBeLessThanOrEqual(1500);
    cache.set('large', { data: 'x'.repeat(10000) });
    expect(cache.get('large')).toBeUndefined();
    expect(cache.bytes).toBeLessThanOrEqual(1500);
    cache.clear();
    expect(cache.bytes).toBe(0);
    expect(cache.size).toBe(0);
  });

  it('charges shared arrays once without serializing another complete answer', () => {
    const rows = Array.from({ length: 1000 }, (_, i) => ({ value: i }));
    const cache = new BoundedCache<unknown>(2, 200000);
    cache.set('single', { rows });
    const single = cache.bytes;
    cache.clear();
    cache.set('shared', { activity: { rows }, answer: { rows } });
    expect(cache.bytes).toBeLessThan(single * 1.1);
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    cache.set('cycle', cyclic);
    expect(cache.size).toBe(2);
  });

  it('bounds distinct small queries and subtracts replaced entries', () => {
    const cache = new BoundedCache<string>(4, 10000);
    for (let i = 0; i < 1000; i++) cache.set(String(i), 'small');
    expect(cache.size).toBe(4);
    const before = cache.bytes;
    cache.set('999', 's');
    expect(cache.bytes).toBeLessThan(before);
    expect(cache.get('999')).toBe('s');
  });
});
