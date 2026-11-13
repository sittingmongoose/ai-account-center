import { describe, expect, it } from 'bun:test';
import {
  buildStrip,
  pinnedProblems,
  type StripInputs,
} from '../../../../src/web-server/status/status-view-strip';
import type {
  PinnedUpdateView,
  ProjectView,
  Source,
} from '../../../../src/web-server/status/status-view-types';
import { NOW } from './status-test-helpers';

const source = (id: string, label: string, state: string, detail = 'Read.'): Source =>
  ({
    id,
    label,
    state,
    asOf: null,
    ageSeconds: null,
    staleAfterSeconds: null,
    detail,
  }) as Source;

const reportsOk = source('agent-reports', 'Agent reports', 'ok', '2 threads read.');

function pin(over: Partial<PinnedUpdateView> & { title: string }): PinnedUpdateView {
  return {
    id: over.id ?? over.title.toLowerCase().replace(/\W+/g, '-'),
    machine: 'vm',
    who: null,
    reportState: 'fresh',
    percent: null,
    percentNote: null,
    now: null,
    blocked: { text: 'None.', waitingOnYou: false },
    looping: { answer: 'no', text: 'No.', task: null, rounds: 0 },
    left: null,
    done: null,
    updated: null,
    reportNote: null,
    staleNote: null,
    unreadableNote: null,
    summary: null,
    ...over,
  };
}

const panel = (updates: PinnedUpdateView[]) => ({
  asOfMs: NOW,
  head: `${updates.length} pinned in T3`,
  counts: { pinned: updates.length, looping: 0, waitingOnYou: 0 },
  machineProblems: [],
  updates,
});

function inputs(over: Partial<StripInputs> = {}): StripInputs {
  return {
    questions: 0,
    decisions: 0,
    reports: reportsOk,
    pins: pinnedProblems(panel([])),
    projects: [],
    done: null,
    blocked: 0,
    overLimitBudgets: 0,
    sources: [reportsOk],
    ...over,
  };
}

describe('Status summary strip', () => {
  it('turns the owner case into counts, the worst pinned thread named once (2026-11-12)', () => {
    const pins = pinnedProblems(
      panel([
        pin({
          title: 'Improve the PM page',
          looping: {
            answer: 'yes',
            text: 'Yes. P-B2 failed review 3 times.',
            task: 'P-B2',
            rounds: 3,
          },
        }),
        pin({
          title: 'Build PMConcept7',
          looping: { answer: 'yes', text: 'Yes.', task: 'F-07', rounds: 2 },
          blocked: { text: 'Waiting on you: pick the reviewers.', waitingOnYou: true },
        }),
        pin({
          title: 'Redesign PM',
          blocked: { text: 'Waiting on you: approve.', waitingOnYou: true },
        }),
        pin({ title: 'Tools', blocked: { text: 'Waiting on you: a key.', waitingOnYou: true } }),
      ])
    );
    const strip = buildStrip(
      inputs({
        questions: 2,
        pins,
        done: { done: 51, of: 83 },
        blocked: 3,
        sources: [
          source(
            'agent-reports',
            'Agent reports',
            'unreadable',
            '1 file unreadable since 5:17 PM.'
          ),
          source('machines', 'Machines', 'ok'),
        ],
      })
    );
    expect(strip.items.map((i) => [i.key, i.value, i.tone, i.section])).toEqual([
      ['questions', '2', 'warn', 'questions'],
      ['looping', '2', 'bad', 'pinned'],
      ['waiting', '3', 'warn', 'pinned'],
      ['blocked', '3', 'warn', null],
      ['done', '51/83', null, null],
      ['sources', '1', 'warn', null],
    ]);
    // No list of titles in the counts: the labels are short words.
    for (const item of strip.items) {
      expect(item.label.length).toBeLessThanOrEqual(24);
      expect(item.short.length).toBeLessThanOrEqual(item.label.length);
      expect(item.label).not.toContain('(');
    }
    // The worst is the loop with the most failing rounds, with its task.
    expect(strip.worst).toEqual({
      state: 'looping',
      word: 'Looping',
      tone: 'bad',
      threadId: 'improve-the-pm-page',
      title: 'Improve the PM page',
      task: 'P-B2',
      detail: 'Yes. P-B2 failed review 3 times.',
    });
    // Each count's sentence says the whole fact for hover and tap.
    const detail = Object.fromEntries(strip.items.map((i) => [i.key, i.detail]));
    expect(detail.looping).toBe(
      '2 pinned threads are looping: Improve the PM page (task P-B2), Build PMConcept7 (task F-07).'
    );
    expect(detail.waiting).toBe(
      '3 pinned threads wait on you: Build PMConcept7, Redesign PM, Tools.'
    );
    expect(detail.done).toBe('51 of 83 items landed or done (dropped items are not counted).');
    // The narrow words fit a phone row: "3 wait on you", "1 waits on you".
    expect(strip.items.find((i) => i.key === 'waiting')?.short).toBe('wait on you');
    expect(detail.sources).toBe('Not current: Agent reports: 1 file unreadable since 5:17 PM.');
  });

  it('hides zero problem counts, keeps a quiet zero for questions, and names no worst thread', () => {
    const strip = buildStrip(inputs({ done: { done: 4, of: 4 } }));
    expect(strip.items.map((i) => [i.key, i.value, i.tone])).toEqual([
      ['questions', '0', 'quiet'],
      ['done', '4/4', null],
    ]);
    expect(strip.items[0].detail).toBe('No questions for you.');
    expect(strip.worst).toBeNull();
  });

  it('says "–" for what it cannot know, never 0', () => {
    // No reading of the agent reports at all: "no questions" would be a guess.
    const missing = source(
      'agent-reports',
      'Agent reports',
      'missing',
      'Waiting for the first scan.'
    );
    const unknown = buildStrip(
      inputs({
        reports: missing,
        sources: [missing],
        pins: pinnedProblems({ unavailable: 'the aac-status native module is disabled' }),
      })
    );
    expect(unknown.items.map((i) => [i.key, i.value, i.tone, i.section])).toEqual([
      ['questions', '–', 'quiet', 'questions'],
      ['pinned', '–', 'quiet', 'pinned'],
      ['sources', '1', 'warn', null],
    ]);
    expect(unknown.items[0].detail).toBe(
      'Questions for you are unknown: Waiting for the first scan.'
    );
    expect(unknown.items[1].detail).toBe(
      'Pinned threads are unavailable: the aac-status native module is disabled.'
    );
    expect(unknown.worst).toBeNull();
    // A server without the pinned panel says so too.
    const absent = buildStrip(inputs({ pins: pinnedProblems(undefined) }));
    expect(absent.items.find((i) => i.key === 'pinned')?.detail).toBe(
      "Pinned threads are unavailable: this server doesn't send them yet."
    );
    // Cards read before the folder failed are still counted, not hidden.
    const failed = source('agent-reports', 'Agent reports', 'error', 'Could not be listed.');
    const kept = buildStrip(inputs({ questions: 3, reports: failed, sources: [failed] }));
    expect(kept.items[0].value).toBe('3');
  });

  it('names a waiting thread when none loops, and lists blocked items by id', () => {
    const projects = [
      {
        id: 'pm7',
        label: 'PM7',
        counts: null,
        completion: null,
        streams: [
          {
            id: 'f',
            items: [
              { id: 'F-07', title: 'Tray icon sharp', state: 'blocked' },
              { id: 'F-08', title: 'Sticker block', state: 'landed' },
            ],
          },
        ],
      },
    ] as unknown as ProjectView[];
    const strip = buildStrip(
      inputs({
        decisions: 1,
        blocked: 1,
        overLimitBudgets: 2,
        projects,
        pins: pinnedProblems(
          panel([
            pin({ title: 'Quiet' }),
            pin({
              title: 'Redesign PM',
              blocked: { text: 'Waiting on you: approve', waitingOnYou: true },
            }),
          ])
        ),
      })
    );
    expect(strip.items.map((i) => i.key)).toEqual([
      'questions',
      'decisions',
      'waiting',
      'blocked',
      'budgets',
    ]);
    expect(strip.items.find((i) => i.key === 'blocked')?.detail).toBe(
      'Blocked: F-07 Tray icon sharp.'
    );
    expect(strip.items.find((i) => i.key === 'budgets')).toMatchObject({
      value: '2',
      tone: 'bad',
      section: 'spend',
      detail: '2 budgets are at or over their limit (estimate).',
    });
    expect(strip.worst).toMatchObject({
      state: 'waiting',
      word: 'Waiting on you',
      tone: 'warn',
      title: 'Redesign PM',
      task: null,
      detail: 'Waiting on you: approve.',
    });
  });

  it('lists at most four names, then how many more', () => {
    const many = Array.from({ length: 6 }, (_, n) =>
      pin({ title: `T${n}`, blocked: { text: 'Waiting on you.', waitingOnYou: true } })
    );
    const strip = buildStrip(inputs({ pins: pinnedProblems(panel(many)) }));
    expect(strip.items.find((i) => i.key === 'waiting')?.detail).toBe(
      '6 pinned threads wait on you: T0, T1, T2, T3 and 2 more.'
    );
  });
});
