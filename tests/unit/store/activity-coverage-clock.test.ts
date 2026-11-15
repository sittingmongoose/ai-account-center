import { expect, it } from 'bun:test';
import { coverageOf, gridFromStore } from '../../../src/store/activity-coverage';
import type { ActivitySources } from '../../../src/store/usage-report';

it('history fallback dates cached usage by the full read, and keeps every scanned tool', () => {
  const before = Date.parse('2026-11-14T20:00:00Z'),
    attempt = before + 3600000;
  const history = coverageOf({
    computers: [],
    sources: [
      {
        computer: 'nas1',
        tool: 'codex',
        state: 'partial',
        cause: 'unreadable_folder',
        lastReadMs: attempt,
        lastCompleteMs: before,
        oldestMs: null,
        badSinceMs: attempt,
      },
      {
        computer: 'ubuntu',
        tool: 'devin',
        state: 'partial',
        cause: 'read_failed',
        lastReadMs: attempt,
        lastCompleteMs: null,
        oldestMs: null,
        badSinceMs: attempt,
      },
      {
        computer: 'alexis',
        tool: 'cursor',
        state: 'installed_no_logs',
        cause: null,
        lastReadMs: attempt,
        lastCompleteMs: attempt,
        oldestMs: null,
        badSinceMs: null,
      },
    ],
  } as ActivitySources);
  const grid = gridFromStore(history);
  expect(grid.find((s) => s.tool === 'codex')).toMatchObject({
    state: 'cached',
    lastScanAt: new Date(before).toISOString(),
    cause: 'unreadable_folder',
  });
  expect(grid.find((s) => s.tool === 'devin')).toMatchObject({
    state: 'cached',
    lastScanAt: null,
    cause: 'read_failed',
  });
  expect(grid.find((s) => s.tool === 'cursor')?.state).toBe('installed_no_logs');
});

it('history fallback explains causes as sentences rather than enum identifiers', () => {
  for (const [cause, sentence] of [
    ['unreadable_folder', 'a folder searched for logs could not be read'],
    ['ssh_unreachable', 'the computer did not answer over ssh'],
    ['read_failed', 'its logs were found but could not be read'],
  ]) {
    const history = coverageOf({
      computers: [],
      sources: [
        {
          computer: 'nas1',
          tool: 'codex',
          state: 'partial',
          cause,
          lastReadMs: null,
          lastCompleteMs: null,
          oldestMs: null,
          badSinceMs: null,
        },
      ],
    } as ActivitySources);
    expect(gridFromStore(history)[0].detail).toBe(sentence);
  }
});
