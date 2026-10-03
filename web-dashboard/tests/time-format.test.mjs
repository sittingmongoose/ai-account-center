// Every displayed time follows the display zone: validation, formatting, and DST-safe calendar math.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULT_DISPLAY_TIME_ZONE,
  displayFormat,
  displayZoneName,
  getDisplayTimeZone,
  isDisplayTimeZone,
  lazyFormat,
  setDisplayTimeZone,
  zonedAddDays,
  zonedDayStart,
  zonedHour,
  zonedHourStart,
  zonedMonthStart,
  zonedParts,
  zonedWeekday,
} from '../public/time-format.mjs';

test('the display zone defaults to America/New_York and only takes zones Intl accepts', () => {
  assert.equal(getDisplayTimeZone(), DEFAULT_DISPLAY_TIME_ZONE);
  assert.equal(DEFAULT_DISPLAY_TIME_ZONE, 'America/New_York');
  assert.equal(isDisplayTimeZone('America/New_York'), true);
  assert.equal(isDisplayTimeZone('UTC'), true);
  assert.equal(isDisplayTimeZone('Mars/Olympus'), false);
  assert.equal(isDisplayTimeZone(''), false);
  assert.equal(isDisplayTimeZone(5), false);
  assert.equal(setDisplayTimeZone('Mars/Olympus'), false);
  assert.equal(getDisplayTimeZone(), 'America/New_York');
  assert.equal(setDisplayTimeZone('UTC'), true);
  assert.equal(getDisplayTimeZone(), 'UTC');
  assert.equal(setDisplayTimeZone('America/New_York'), true);
});

test('formatters render in the display zone, whatever the machine zone', () => {
  setDisplayTimeZone('America/New_York');
  // 2026-10-01T00:30Z is still Sep 30 in New York (EDT)
  const t = Date.parse('2026-10-01T00:30:00Z');
  assert.equal(displayFormat({ month: 'short', day: 'numeric' }).format(new Date(t)), 'Sep 30');
  assert.equal(lazyFormat({ hour: 'numeric', minute: '2-digit' }).format(new Date(t)), '8:30 PM');
  setDisplayTimeZone('UTC');
  assert.equal(displayFormat({ month: 'short', day: 'numeric' }).format(new Date(t)), 'Oct 1');
  assert.equal(displayZoneName(t), 'UTC');
  setDisplayTimeZone('America/New_York');
  assert.equal(displayZoneName(t), 'EDT');
  assert.equal(displayZoneName(Date.parse('2026-01-15T12:00:00Z')), 'EST');
});

test('the zoned calendar matches wall arithmetic, across a DST change', () => {
  setDisplayTimeZone('America/New_York');
  // a plain day: midnight, hour truncation, weekday (Mon 0 .. Sun 6)
  const noon = Date.parse('2026-10-01T16:00:00Z'); // Thu 12:00 EDT
  assert.equal(zonedDayStart(noon), Date.parse('2026-10-01T04:00:00Z'));
  assert.equal(zonedHour(noon), 12);
  assert.equal(zonedWeekday(noon), 3);
  assert.equal(zonedHourStart(noon), Date.parse('2026-10-01T16:00:00Z'));
  assert.equal(zonedMonthStart(noon), Date.parse('2026-10-01T04:00:00Z'));
  assert.deepEqual(zonedParts(noon), { year: 2026, month: 10, day: 1, hour: 12, minute: 0, second: 0, weekdaySun0: 4 });
  // over the Nov 1 fall-back (25-hour Sunday): each midnight is exact
  const sat = Date.parse('2026-10-31T15:00:00Z'); // Sat 11:00 EDT
  assert.equal(zonedAddDays(sat, 1), Date.parse('2026-11-01T04:00:00Z')); // Sun 00:00 EDT
  assert.equal(zonedAddDays(sat, 2), Date.parse('2026-11-02T05:00:00Z')); // Mon 00:00 EST
  assert.equal(zonedDayStart(Date.parse('2026-11-01T20:00:00Z')), Date.parse('2026-11-01T04:00:00Z'));
  // and over the Mar 8 spring-forward (23-hour Sunday)
  const fri = Date.parse('2026-03-06T15:00:00Z'); // Fri 10:00 EST
  assert.equal(zonedAddDays(fri, 2), Date.parse('2026-03-08T05:00:00Z')); // Sun 00:00 EST
  assert.equal(zonedAddDays(fri, 3), Date.parse('2026-03-09T04:00:00Z')); // Mon 00:00 EDT
  // month starts land on the 1st in-zone
  assert.equal(zonedMonthStart(Date.parse('2026-10-20T12:00:00Z')), Date.parse('2026-10-01T04:00:00Z'));
  setDisplayTimeZone('UTC');
  assert.equal(zonedDayStart(noon), Date.parse('2026-10-01T00:00:00Z'));
  setDisplayTimeZone('America/New_York');
});
