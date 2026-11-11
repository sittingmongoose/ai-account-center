import { expect, test } from 'bun:test';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';
import { threadDoc, tasksDoc } from './status-test-helpers';
const extra = { title: 'Build', now: 'Checking.', next: 'Land', stages: [{ label: 'Build', state: 'done' }, { label: 'Land', state: 'next' }], deliverables: [{ id: 'report', name: 'Report', kind: 'report', version: 'v2', updatedAt: '2026-11-11T16:00:00Z', supersedes: 'v1', media: [] }] };
test('thread and tasks retain additive v2 fields', () => {
  for (const kind of ['thread', 'tasks'] as const) {
    const doc = kind === 'thread' ? threadDoc('vm.lead') : tasksDoc('vm.lead', []);
    const result = parseReportFile(kind as 'thread', Buffer.from(JSON.stringify({ ...doc, ...extra })), 'vm.lead');
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.doc).toMatchObject(extra);
  }
});
