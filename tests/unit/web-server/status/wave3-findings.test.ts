import { expect, test } from 'bun:test';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';
import { runDoc } from '../scoreboard/scoreboard-test-helpers';
test('review finding severities can be explicitly unknown', () => {
  const doc = runDoc('vm.lead', 'review-null');
  doc.review = { of: 'build', verdict: 'fail', findings: { high: null, medium: 0, low: null }, overturned: false, disputed: false, by: null };
  const parsed = parseReportFile('run', Buffer.from(JSON.stringify(doc)), 'vm.lead');
  expect(parsed.ok).toBe(true);
  if (parsed.ok) expect(parsed.value.doc.review?.findings.high).toBeNull();
});
