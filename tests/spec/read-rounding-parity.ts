/** Numeric admission through the production bridge and Status validator, fixture inputs only. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import type { Request } from 'express';
import type { RustService } from '../../src/web-server/rust-service';
import { rustStatusInput, isRustStatusView } from '../../src/web-server/rust-read-input';
import { buildStatusView } from '../../src/web-server/status/status-view';

export async function checkRoundingParity(service: RustService, fixture: any): Promise<void> {
  const rows: any[] = JSON.parse(
    fs.readFileSync(new URL('./read-numbers.json', import.meta.url), 'utf8')
  );
  const req = {
    method: 'GET',
    sessionID: 'rounding-fixture',
    session: { authenticated: true },
  } as Request;
  let checks = 0;
  async function check(input: any, label: string, fallback = false) {
    const expected = buildStatusView(input);
    const before = { ...service.stats };
    let legacyCalls = 0;
    const actual: any = await service.compose(
      req,
      '/api/status',
      () => rustStatusInput(input),
      () => {
        legacyCalls++;
        return expected;
      },
      isRustStatusView
    );
    assert.equal(
      actual.usage.accounts[0].usedPct,
      expected.usage.accounts[0].usedPct,
      `${label}: accepted numeric reading differs`
    );
    assert.ok(
      JSON.stringify(actual) === JSON.stringify(expected),
      `${label}: public serialized bytes differ`
    );
    assert.equal(legacyCalls, fallback ? 1 : 0, `${label}: legacy admission`);
    assert.equal(
      service.stats.fallbacks - before.fallbacks,
      fallback ? 1 : 0,
      `${label}: logged fallback counter`
    );
    assert.equal(
      service.stats.served - before.served,
      fallback ? 0 : 1,
      `${label}: actual Rust response`
    );
    checks++;
  }
  // The new Node table drives review M1; this must fail at bc3c1f496.
  const witness = rows.find(
    (row) => row.kind === 'math-round' && row.op === 'tenth' && row.value === 0.049999999999999996
  );
  assert.ok(witness, 'Node table must carry the review boundary neighbor');
  assert.equal(witness.expectedBits, '0000000000000000');
  for (const timeZone of ['UTC', 'America/New_York', 'invalid-fixture-zone']) {
    const input = structuredClone(fixture);
    input.timeZone = timeZone;
    input.machines.vm.data.load1 = witness.value;
    input.usage.accounts[0].windows[0].usedPercent = witness.value;
    input.machines.vm.data.memAvailableBytes = witness.value * 1073741824;
    input.machines.vm.data.disk.freeBytes = witness.value * 1073741824;
    input.quota.good.doc.periods.day.rows[0].value = 0.004999999999999999;
    input.quota.good.doc.periods.day.rows[1].value = witness.value;
    input.quota.good.doc.periods.week.coverage = 0.004999999999999999;
    input.quota.good.doc.budgets[1].percent = 0.49999999999999994;
    await check(input, `review M1 in ${timeZone}`);
  }
  const vectors = (op: string, max = 1e12) =>
    rows.filter(
      (row) => row.kind === 'math-round' && row.op === op && row.value >= 0 && row.value <= max
    );
  for (const op of ['tenth', 'hundredth', 'integer']) {
    const points = vectors(op);
    for (let offset = 0; offset < points.length; offset += 64) {
      const batch = points.slice(offset, offset + 64);
      const input = structuredClone(fixture);
      input.timeZone = 'UTC';
      if (op === 'tenth') {
        input.machines.vm.data.load1 = batch[0].value;
        input.usage.accounts[0].windows = batch.map((row, index) => ({
          ...fixture.usage.accounts[0].windows[0],
          label: `boundary-${offset + index}`,
          usedPercent: row.value,
        }));
      }
      if (op === 'tenth' || op === 'hundredth') {
        const seed = fixture.quota.good.doc.periods.day.rows[op === 'tenth' ? 1 : 0];
        input.quota.good.doc.periods.day.rows = batch.map((row) => ({ ...seed, value: row.value }));
      } else {
        const seed = fixture.quota.good.doc.budgets[1];
        input.quota.good.doc.budgets = batch.map((row) => ({ ...seed, percent: row.value }));
      }
      await check(input, `${op} neighbor batch ${offset}`);
    }
  }
  // Coverage shares exercise the separate percent wording; each source period carries one.
  const shares = vectors('percent', 0.5);
  for (let offset = 0; offset < shares.length; offset += 3) {
    const input = structuredClone(fixture);
    for (const [index, id] of ['day', 'week', 'window'].entries()) {
      if (shares[offset + index])
        input.quota.good.doc.periods[id].coverage = shares[offset + index].value;
    }
    await check(input, `coverage neighbor batch ${offset}`);
  }
  const unproven = structuredClone(fixture);
  unproven.quota.good.doc.budgets[1].percent = 1e30;
  await check(unproven, 'unproven numeric word', true);
  console.log(
    `${checks} numeric Status admissions: exact Node-neighbor values or explicit logged TS fallback`
  );
}
