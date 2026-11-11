import { expect, test } from 'bun:test';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';
import { item, tasksDoc } from './status-test-helpers';
test('unknown task state keeps the rest of the board and marks that row', () => {
  const board = tasksDoc('vm.lead', [item('good', 'done'), { ...item('bad', 'todo'), state: 'queued' }]);
  const parsed = parseReportFile('tasks', Buffer.from(JSON.stringify(board)), 'vm.lead');
  expect(parsed.ok).toBe(true);
  if (parsed.ok) {
    expect(parsed.value.doc.items).toHaveLength(2);
    expect(parsed.value.doc.items[1].state).toBe('queued');
  }
});
