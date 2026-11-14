// A fixed clock for golden comparisons, imported first: Date.now() and new Date() read 2026-11-14T12:00:00Z and local
// time is UTC, so every word a view builds from the time ("30s ago", "Review valid until ...") is the same on every
// machine and day. Only test files that compare against recorded output import it.
process.env.TZ = 'UTC';

export const FROZEN_NOW = Date.parse('2026-11-14T12:00:00.000Z');
const RealDate = Date;

class FrozenDate extends RealDate {
  constructor(...args) {
    if (args.length === 0) super(FROZEN_NOW);
    else super(...args);
  }

  static now() {
    return FROZEN_NOW;
  }
}

globalThis.Date = FrozenDate;
