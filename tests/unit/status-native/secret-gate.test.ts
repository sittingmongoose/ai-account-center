import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';
import { openStatusHandle } from '../../../src/status-native/pinned';
import type { ReaderFs } from '../../../src/web-server/status/agent-reports-reader';
import { StatusDocsReader } from '../../../src/web-server/status/status-docs';

/** The module `bun run status:build` leaves behind; the gate check needs the real reader. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;

const NOW = Date.parse('2026-11-08T12:00:00Z');

/**
 * One complete status@1 document with the summary given; the writer matches the fixture
 * share. A token-shaped fixture is joined at runtime so this source file holds none whole.
 */
function statusSource(summary: string): string {
  return JSON.stringify({
    schema: 'agent-reports/status@1',
    writer: 'vm.gate',
    seq: 9,
    writtenAt: '2026-11-08T12:00:00Z',
    t3Thread: 'fixture-thread',
    summary,
  });
}

/** A fixture share holding one writer's status.json whose bytes the test swaps. */
function fixtureShare() {
  let bytes = Buffer.from(statusSource('Sweeping.'));
  const dirent = (name: string, directory: boolean): fs.Dirent =>
    ({ name, isDirectory: () => directory }) as unknown as fs.Dirent;
  const fixtureFs: ReaderFs = {
    readdir: async (dir) =>
      dir.endsWith('/writers') ? [dirent('vm.gate', true)] : [dirent('status.json', false)],
    stat: async () => ({ mtimeMs: 1, size: 200, isFile: () => true }) as unknown as fs.Stats,
    readFile: async () => bytes,
  };
  return {
    fs: fixtureFs,
    write: (source: string) => {
      bytes = Buffer.from(source, 'utf8');
    },
  };
}

/**
 * The single-gate regression (owner decision 2026-11-09): the TypeScript reader checks every
 * status report for secrets before the native module reads it, and the native module no
 * longer keeps a copy of that check. These tests run against the built module on purpose —
 * a report the TypeScript check refuses must be refused even though the module underneath
 * would now take it, and a clean report must still come back from the module unchanged.
 */
describe('the one secret gate, in front of the native module', () => {
  withModule('a secret report is refused by TypeScript and never reaches the module or the composer', async () => {
    const handle = openStatusHandle({ file: PREBUILT });
    if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
    const share = fixtureShare();
    const read = new StatusDocsReader({
      root: () => '/fixture',
      now: () => NOW,
      handle: () => handle,
      fs: share.fs,
    });
    // The round-2 witness: a bearer token with U+FEFF as its separator.
    share.write(statusSource(`Bearer\uFEFF${'i'.repeat(24)}`));
    await read.scan();
    let state = read.snapshot();
    expect(state.problems[0]?.reason).toBe(
      'status.json refused: it contains what looks like a bearer token'
    );
    // A non-ASCII letter holding the API-key boundary (the original probe's witness).
    share.write(statusSource(`密sk-${'A'.repeat(24)}`));
    await read.scan();
    state = read.snapshot();
    // The TypeScript refusal, with the TypeScript reason, named on the file.
    expect(state.docs).toEqual([]);
    expect(state.problems).toHaveLength(1);
    expect(state.problems[0]?.reason).toBe(
      'status.json refused: it contains what looks like an API key'
    );

    // What the composer would receive from this reader: no status row at all, so the pinned
    // thread composes as having no status document.
    const composed = JSON.parse(
      await handle.call(
        'compose',
        JSON.stringify({
          nowMs: NOW,
          utcOffsetMinutes: 0,
          pins: [{ machine: 'vm', id: 'fixture-thread', title: 'Fixture thread' }],
          threads: [
            {
              writer: 'vm.gate',
              writtenAt: '2026-11-08T12:00:00Z',
              writtenAtMs: NOW,
              seq: 1,
              t3Thread: 'fixture-thread',
              project: 'fixture',
              stream: 'gate',
              state: 'working',
            },
          ],
          statuses: state.docs.map((row) => ({
            writer: row.writer,
            writtenAt: row.doc.writtenAt,
            writtenAtMs: row.writtenAtMs,
            seq: row.seq,
            t3Thread: row.doc.t3Thread,
            summary: row.doc.summary,
            problem: row.problem?.reason ?? null,
            problemSinceMs: row.problem?.since ?? null,
          })),
        })
      )
    ) as { updates: Array<{ now: string | null }> };
    expect(composed.updates).toHaveLength(1);
    // `now` is the status document's summary (the thread carries no `doing` here): null
    // means the composer was given no status row at all — the report never reached it.
    expect(composed.updates[0].now).toBeNull();
  });

  withModule('a clean report reaches the module and flows through unchanged', async () => {
    const handle = openStatusHandle({ file: PREBUILT });
    if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
    const share = fixtureShare();
    const summary = '清扫 stream F，批次 9 — 😀 done, 收尾 left.';
    share.write(statusSource(summary));
    const read = new StatusDocsReader({
      root: () => '/fixture',
      now: () => NOW,
      handle: () => handle,
      fs: share.fs,
    });
    await read.scan();
    const state = read.snapshot();
    expect(state.problems).toEqual([]);
    expect(state.docs).toHaveLength(1);
    expect(state.docs[0].doc.summary).toBe(summary);
    expect(state.docs[0].problem).toBeNull();
    expect(state.docs[0].seq).toBe(9);

    // And the composer shows that summary as the thread's `now` word.
    const composed = JSON.parse(
      await handle.call(
        'compose',
        JSON.stringify({
          nowMs: NOW,
          utcOffsetMinutes: 0,
          pins: [{ machine: 'vm', id: 'fixture-thread', title: 'Fixture thread' }],
          threads: [
            {
              writer: 'vm.gate',
              writtenAt: '2026-11-08T12:00:00Z',
              writtenAtMs: NOW,
              seq: 1,
              t3Thread: 'fixture-thread',
              project: 'fixture',
              stream: 'gate',
              state: 'working',
            },
          ],
          statuses: state.docs.map((row) => ({
            writer: row.writer,
            writtenAt: row.doc.writtenAt,
            writtenAtMs: row.writtenAtMs,
            seq: row.seq,
            t3Thread: row.doc.t3Thread,
            summary: row.doc.summary,
            problem: row.problem?.reason ?? null,
            problemSinceMs: row.problem?.since ?? null,
          })),
        })
      )
    ) as { updates: Array<{ now: string | null }> };
    expect(composed.updates[0]?.now).toBe(summary);
  });
});
