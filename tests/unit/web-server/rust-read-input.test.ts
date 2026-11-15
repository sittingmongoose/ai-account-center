import type { Request } from 'express';
import { RustService } from '../../../src/web-server/rust-service';
import { expect, it } from 'bun:test';
import { rustReadInput } from '../../../src/web-server/rust-read-input';

it('refuses non-finite numbers before JSON can replace them with null', () => {
  for (const value of [NaN, Infinity, -Infinity]) {
    for (const input of [value, { usage: [{ usedPercent: value }] }, new Map([['usage', value]])]) {
      expect(() => rustReadInput(input, 'UTC')).toThrow('non-finite');
    }
  }
});

it('preserves finite numbers, nulls, maps and date facts', () => {
  const input = { usage: new Map([['value', 0.25]]), absent: null, at: '2026-11-14T20:00:00Z' };
  const result = rustReadInput(input, 'UTC') as any;
  expect(result.data).toEqual({ usage: { value: 0.25 }, absent: null, at: input.at });
  expect(result.presentation.epochs[input.at]).toBe(Date.parse(input.at));
});

it.each([NaN, Infinity, -Infinity])(
  'compose falls back before forwarding a non-finite input: %s',
  async (usedPercent) => {
    const service = new RustService('on');
    let forwarded = false;
    let legacyCalls = 0;
    // Isolate the input boundary after the principal gate, without a process or transport.
    (service as any).forwardable = () => ({ session: 'fixture' });
    (service as any).forward = async () => {
      forwarded = true;
      return { status: 200, body: { source: 'rust' } };
    };
    const expected = { source: 'typescript', atLimit: usedPercent === Infinity };
    const actual = await service.compose(
      { method: 'GET' } as Request,
      '/api/status',
      () => rustReadInput({ usage: { usedPercent } }, 'UTC'),
      () => {
        legacyCalls++;
        return expected;
      },
      () => true
    );
    expect(actual).toEqual(expected);
    expect(legacyCalls).toBe(1);
    expect(forwarded).toBe(false);
    expect(service.stats).toMatchObject({ served: 0, fallbacks: 1 });
    service.stop();
  }
);
