import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';
import { openStatusHandle } from '../../../src/status-native/pinned';
import { composeByModel } from '../../../src/web-server/scoreboard/by-model';
import { composeSpendByStream } from '../../../src/web-server/status/spend-by-stream';

/**
 * L3: the native Spend and Scoreboard texts round with JavaScript's own `Number.prototype.toFixed`.
 *
 * The module's old copy compared n/10^places with the value in binary64 and misread values beside a tie: 0.185 to
 * two places wrote "0.19" where every engine writes "0.18". Each case goes through the call the dashboard makes
 * today (the real module behind `composeSpendByStream` and `composeByModel`), and the expected words are computed
 * here with `toFixed` on the same double the composer formats.
 */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../crates/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;
const FIXTURES = path.join(import.meta.dir, '../../../crates/aac-status/tests/fixtures');
const fixture = (name: string): Record<string, any> =>
  JSON.parse(fs.readFileSync(path.join(FIXTURES, name), 'utf8')) as Record<string, any>;

/** Sets every measure named `name` that has a value (and, for `unit`, only that unit) to `value`. */
function setMeasures(view: unknown, name: string, value: number, unit?: string): number {
  let set = 0;
  const walk = (node: unknown): void => {
    if (Array.isArray(node)) node.forEach(walk);
    else if (node && typeof node === 'object') {
      const record = node as Record<string, any>;
      const measure = record[name];
      if (
        measure &&
        typeof measure === 'object' &&
        measure.value !== null &&
        (!unit || measure.unit === unit)
      ) {
        measure.value = value;
        set++;
      }
      Object.values(record).forEach(walk);
    }
  };
  walk(view);
  return set;
}

function handle() {
  const opened = openStatusHandle({ file: PREBUILT });
  if ('unavailable' in opened) throw new Error(opened.unavailable);
  return opened;
}

/** Every `{label, value}` measure or `{text}` cell in a composed answer. */
function words(
  value: unknown,
  out: Array<Record<string, unknown>> = []
): Array<Record<string, unknown>> {
  if (Array.isArray(value)) for (const item of value) words(item, out);
  else if (value && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    if (typeof record.value === 'string' || typeof record.text === 'string') out.push(record);
    for (const item of Object.values(record)) words(item, out);
  }
  return out;
}

// Values where the old binary64 copy and toFixed disagree: [value, places, old copy's text].
const SCOREBOARD_COSTS: Array<[number, string]> = [
  [0.185, '0.19'],
  [0.155, '0.16'],
  [0.215, '0.22'],
  [1.45, '1.5'],
  [4.05, '4.1'],
];
const SCOREBOARD_SPEEDS: Array<[number, string]> = [
  [1.45, '1.5 s'],
  [4.05, '4.1 s'],
];
const SPEND_FIVE_HOUR_PERCENTS: Array<[number, string]> = [
  [145, '1.5×'],
  [405, '4.1×'],
  [555, '5.6×'],
];

describe('native toFixed parity (L3)', () => {
  withModule('Scoreboard cost per item rounds as toFixed: 0.185 is 0.18', async () => {
    for (const [cost, old] of SCOREBOARD_COSTS) {
      const view = fixture('scoreboard-view.json');
      expect(setMeasures(view, 'cost', cost)).toBeGreaterThan(0);
      const answer = await composeByModel(view, { handle: handle() });
      expect(answer).not.toBeNull();
      // scoreboard.rs round1: one decimal from 1 up, two below it.
      const want = cost >= 1 ? cost.toFixed(1) : cost.toFixed(2);
      const costs = words(answer)
        .filter((m) => String(m.label).startsWith('Cost per'))
        .map((m) => m.value);
      expect(costs.length).toBeGreaterThan(0);
      expect(new Set(costs)).toEqual(new Set([want]));
      expect(costs).not.toContain(old);
    }
  });

  withModule('Scoreboard speed under five seconds rounds as toFixed: 1.45 s is 1.4 s', async () => {
    for (const [seconds, old] of SCOREBOARD_SPEEDS) {
      const view = fixture('scoreboard-view.json');
      // Per-item speeds under five seconds are written with one decimal (scoreboard.rs `seconds`).
      expect(setMeasures(view, 'speed', seconds, 's/item')).toBeGreaterThan(0);
      const answer = await composeByModel(view, { handle: handle() });
      expect(answer).not.toBeNull();
      const values = words(answer).map((m) => m.value);
      expect(values).toContain(`${seconds.toFixed(1)} s`);
      expect(values).not.toContain(old);
    }
  });

  withModule(
    'Status spend by stream writes a five-hour multiple as toFixed: 145% is 1.4×',
    async () => {
      for (const [percent, old] of SPEND_FIVE_HOUR_PERCENTS) {
        const input = fixture('spend-input.json');
        const row = input.spend.periods[0].streams[0].rows.find(
          (r: Record<string, unknown>) => r.account === 'claude:gmail' && r.meter === 'five_hour'
        );
        row.value = percent;
        const panel = await composeSpendByStream(input as never, { handle: handle() });
        expect((panel as { state: string }).state).toBe('ok');
        // The composer's share: the running sum of value / 100 over this column's rows (here one).
        const share = 0 + percent / 100;
        const texts = words(panel).map((m) => m.text);
        expect(texts).toContain(`${share.toFixed(1)}×`);
        expect(texts).not.toContain(old);
      }
    }
  );
});
