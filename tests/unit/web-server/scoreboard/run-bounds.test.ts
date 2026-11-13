/**
 * The run bounds the reader and the published run schema agree on (2026-11-13): opaque task handles up
 * to 512 characters, evidence labels up to 300, and run local ids up to 128, each kept exactly as
 * written. Before, 699 real runs were refused at 120-character caps the schema never published.
 * Synthetic documents only, shaped like the refused ones (a T3 delegated-task handle, a long label).
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import {
  RUN_EVIDENCE_LABEL_MAX,
  RUN_ID_MAX,
  RUN_LOCAL_ID_MAX,
  RUN_TASK_ID_MAX,
} from '../../../../src/web-server/status/run-schema';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';
import { NOW, tempDirs } from '../status/status-test-helpers';
import { runDoc, writeRun } from './scoreboard-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const WRITER = 'vm.pm7-build';
const bytes = (doc: unknown) => Buffer.from(JSON.stringify(doc));

/** A delegated-task handle shaped like T3's (`node:delegated-task:command...`), `length` characters. */
function taskHandle(length: number): string {
  const head = 'node:delegated-task:command:agent-run:claude-opus-5-5:';
  return head + 'x'.repeat(length - head.length);
}

function parsed(doc: unknown) {
  return parseReportFile('run', bytes(doc), WRITER);
}

describe('run bounds', () => {
  it('accepts an opaque task handle of 121 to 512 characters and keeps it whole', () => {
    for (const length of [121, 129, 272, RUN_TASK_ID_MAX]) {
      const id = taskHandle(length);
      const result = parsed(runDoc(WRITER, `delegate-${length}`, { task: { id, type: null, title: 'Delegate' } }));
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value.doc.task.id).toBe(id);
    }
  });

  it('refuses a task handle past 512 characters, saying it is the length', () => {
    const result = parsed(
      runDoc(WRITER, 'delegate-long', { task: { id: taskHandle(513), type: null, title: 'Delegate' } })
    );
    expect(result).toEqual({ ok: false, reason: 'task.id is too long: 513 characters, over the 512 allowed' });
  });

  it('accepts evidence labels up to 300 characters and keeps them whole', () => {
    const label = `Round 2 review of the settings managers, ${'with every file it read '.repeat(8)}`.slice(0, 224);
    const longest = 'L'.repeat(RUN_EVIDENCE_LABEL_MAX);
    const result = parsed(
      runDoc(WRITER, 'review-1', {
        evidence: [
          { label, path: '/mnt/Cursor/share/example/review.md' },
          { label: longest, path: '/mnt/Cursor/share/example/more.md' },
        ],
      })
    );
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.value.doc.evidence.map((e) => e.label)).toEqual([label, longest]);
    expect(
      parsed(runDoc(WRITER, 'review-2', { evidence: [{ label: 'L'.repeat(301), path: '/x' }] }))
    ).toEqual({ ok: false, reason: 'evidence[0].label is too long: 301 characters, over the 300 allowed' });
  });

  it('still refuses evidence written as plain strings', () => {
    const result = parsed(runDoc(WRITER, 'lane-h', { evidence: ['/home/example/H-REPORT.md'] }));
    expect(result).toEqual({ ok: false, reason: 'evidence[0] is missing or not valid' });
  });

  it('accepts a run local id up to 128 characters and refuses longer, saying it is the length', () => {
    const local73 = `obs-20261025-g4-dry-run-third-vote-${'a'.repeat(31)}-92e6b2`;
    expect(local73).toHaveLength(73);
    expect(parsed(runDoc(WRITER, local73)).ok).toBe(true);
    expect(parsed(runDoc(WRITER, 'r'.repeat(RUN_LOCAL_ID_MAX))).ok).toBe(true);
    expect(parsed(runDoc(WRITER, 'r'.repeat(RUN_LOCAL_ID_MAX + 1)))).toEqual({
      ok: false,
      reason: `id is too long: the part after "${WRITER}--" is 129 characters, over the 128 allowed`,
    });
    expect(RUN_ID_MAX).toBe(194);
  });

  it('names which id rule a run breaks: the writer prefix or the grammar', () => {
    // Real refusals had a bare `<name>-<date>` id with no writer in front.
    expect(parsed({ ...runDoc(WRITER, 'x'), id: 'pm7-build-20261111' })).toEqual({
      ok: false,
      reason: `id does not name its writer: it must start with "${WRITER}--"`,
    });
    expect(parsed({ ...runDoc(WRITER, 'x'), id: 'vm.other--R1' })).toEqual({
      ok: false,
      reason: `id does not name its writer: it must start with "${WRITER}--"`,
    });
    expect(parsed({ ...runDoc(WRITER, 'x'), id: `${WRITER}--bad id` })).toMatchObject({
      ok: false,
      reason: expect.stringContaining('id is not valid'),
    });
    expect(parsed({ ...runDoc(WRITER, 'x'), id: `${WRITER}--` })).toMatchObject({ ok: false });
  });

  it('the runs reader reads a run whose file name is past 100 characters', async () => {
    const root = dirs.make();
    const local = `obs-20261031-pilot-2-gold-astra-judges-two-plan-sections-${'b'.repeat(40)}`;
    const doc = runDoc(WRITER, local, {
      task: { id: taskHandle(272), type: 'review', title: 'Judge' },
    });
    const file = writeRun(root, doc);
    expect(path.basename(file).length).toBeGreaterThan(100);
    const runs = new RunsReader({ root: () => root, now: () => NOW });
    await runs.scan();
    const state = runs.snapshot();
    expect(state.problems).toEqual([]);
    expect(state.runs.map((r) => [r.doc.id, r.doc.task.id])).toEqual([[doc.id, taskHandle(272)]]);
  });

  it('publishes the same bounds in docs/agent-reports/schemas/run.json', () => {
    const schema = JSON.parse(
      fs.readFileSync(path.resolve(import.meta.dir, '../../../../docs/agent-reports/schemas/run.json'), 'utf8')
    );
    expect(schema.properties.task.properties.id.maxLength).toBe(RUN_TASK_ID_MAX);
    expect(schema.properties.evidence.items.properties.label.maxLength).toBe(RUN_EVIDENCE_LABEL_MAX);
    expect(schema.properties.id.maxLength).toBe(RUN_ID_MAX);
  });
});
