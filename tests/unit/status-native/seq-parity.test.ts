import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../src/status-native/native-loader';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';
import { openStatusHandle } from '../../../src/status-native/pinned';
import { head } from '../../../src/web-server/status/schema-helpers';
import type { ReaderFs } from '../../../src/web-server/status/agent-reports-reader';
import { StatusDocsReader } from '../../../src/web-server/status/status-docs';

/** The module `bun run status:build` leaves behind; the parity check needs the real reader. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;

const STATUS_SCHEMA = 'agent-reports/status@1';
const SAFE_MAX = '9007199254740991'; // 2^53-1, Number.MAX_SAFE_INTEGER

/**
 * A complete status@1 document whose `seq` is the literal digits given. Never built through
 * JSON.stringify's number path: 2^53+1 would round to 2^53 before either reader sees it.
 */
function statusSource(seq: string, summary: string): string {
  return (
    `{"schema":"${STATUS_SCHEMA}","writer":"vm.seq","seq":${seq}` +
    `,"writtenAt":"2026-11-08T12:00:00Z","t3Thread":"fixture-thread","summary":${JSON.stringify(summary)}}`
  );
}

/** The built module's verdict over the document, as the reader's `parseStatus` sees it. */
async function parseNative(handle: NativeStatusHandle, source: string) {
  return JSON.parse(
    await handle.call(
      'parseStatus',
      JSON.stringify({
        writer: 'vm.seq',
        contentBase64: Buffer.from(source, 'utf8').toString('base64'),
      })
    )
  ) as { ok: boolean; reason?: string; doc?: { seq?: number } };
}

describe('the sequence cap across the two readers', () => {
  withModule('a seq above the safe-integer maximum is refused exactly where TypeScript refuses it', async () => {
    const handle = openStatusHandle({ file: PREBUILT });
    if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
    const cases: Array<[string, boolean]> = [
      [SAFE_MAX, true], // 2^53-1: the safe maximum, accepted and held exactly
      ['9007199254740992', false], // 2^53: the first integer JavaScript cannot hold exactly
      ['9007199254740993', false], // 2^53+1: reads as 2^53 there
      ['18446744073709551615', false], // u64::MAX
      ['-1', false],
      ['1.5', false],
      ['4.0', true], // a JSON `4.0` is JavaScript's 4
    ];
    for (const [seq, accepted] of cases) {
      const source = statusSource(seq, 'Sweeping.');
      let tsAccepted = true;
      let tsReason = '';
      try {
        head(JSON.parse(source) as Record<string, unknown>, STATUS_SCHEMA, 'vm.seq');
      } catch (error) {
        tsAccepted = false;
        tsReason = (error as Error).message;
      }
      const rust = await parseNative(handle, source);
      expect(tsAccepted, `${seq}: TypeScript must ${accepted ? 'accept' : 'refuse'}`).toBe(
        accepted
      );
      expect(rust.ok, `${seq}: the native reader must agree with TypeScript`).toBe(tsAccepted);
      if (!accepted) {
        expect(tsReason, `${seq}: TypeScript reason`).toBe('seq is missing or not valid');
        expect(rust.reason, `${seq}: native reason`).toBe('seq is missing or not valid');
      }
    }
    // The safe maximum comes back as the exact same number, still a safe integer.
    const max = await parseNative(handle, statusSource(SAFE_MAX, 'Sweeping.'));
    expect(max.doc?.seq).toBe(9007199254740991);
    expect(Number.isSafeInteger(max.doc?.seq)).toBe(true);
  });

  withModule('an unsafe or lower sequence never replaces a read copy through the reader', async () => {
    const handle = openStatusHandle({ file: PREBUILT });
    if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
    const now = () => Date.parse('2026-11-08T12:00:00Z');

    // A fixture share that always lists one writer with one status.json whose bytes the
    // test swaps between scans; `version` picks what readFile returns.
    let version = 0;
    let source = statusSource(SAFE_MAX, 'Revision A');
    const dirent = (name: string, directory: boolean): fs.Dirent =>
      ({ name, isDirectory: () => directory }) as unknown as fs.Dirent;
    const fixtureFs: ReaderFs = {
      readdir: async (dir) => (dir.endsWith('/writers') ? [dirent('vm.seq', true)] : [dirent('status.json', false)]),
      stat: async () => ({ mtimeMs: version + 1, size: 200, isFile: () => true }) as unknown as fs.Stats,
      readFile: async () => Buffer.from(source, 'utf8'),
    };
    const read = new StatusDocsReader({
      root: () => '/fixture',
      now,
      handle: () => handle,
      fs: fixtureFs,
    });

    // The safe maximum is read and held exactly.
    await read.scan();
    let doc = read.snapshot().docs[0];
    expect(doc.seq).toBe(9007199254740991);
    expect(doc.doc.summary).toBe('Revision A');
    expect(doc.problem).toBeNull();

    // A lower safe sequence is not the document: the higher seq stays and the rollback is
    // said (the agent-reports reader's rule), until a same-or-higher seq arrives.
    version += 1;
    source = statusSource('4', 'Revision B');
    await read.scan();
    doc = read.snapshot().docs[0];
    expect(doc.seq).toBe(9007199254740991);
    expect(doc.doc.summary).toBe('Revision A');
    expect(doc.problem?.reason).toContain('seq is lower');

    // 2^53 and 2^53+1 arrive: both refused by both readers, the last good copy stays and
    // the refusal is named on it.
    for (const seq of ['9007199254740992', '9007199254740993']) {
      version += 1;
      source = statusSource(seq, 'Unsafe revision');
      await read.scan();
      doc = read.snapshot().docs[0];
      expect(doc.seq).toBe(9007199254740991);
      expect(doc.doc.summary).toBe('Revision A');
      expect(doc.problem?.reason).toContain('seq is missing or not valid');
    }

    // The safe maximum again with new text is read: same seq, a newer copy, problem cleared.
    version += 1;
    source = statusSource(SAFE_MAX, 'Revision C');
    await read.scan();
    doc = read.snapshot().docs[0];
    expect(doc.seq).toBe(9007199254740991);
    expect(doc.doc.summary).toBe('Revision C');
    expect(doc.problem).toBeNull();
  });

  withModule('a document whose first sequence is already unsafe is refused, never read', async () => {
    const handle = openStatusHandle({ file: PREBUILT });
    if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
    const dirent = (name: string, directory: boolean): fs.Dirent =>
      ({ name, isDirectory: () => directory }) as unknown as fs.Dirent;
    const fixtureFs: ReaderFs = {
      readdir: async (dir) => (dir.endsWith('/writers') ? [dirent('vm.seq', true)] : [dirent('status.json', false)]),
      stat: async () => ({ mtimeMs: 1, size: 200, isFile: () => true }) as unknown as fs.Stats,
      readFile: async () =>
        Buffer.from(statusSource('9007199254740993', 'Newer revision'), 'utf8'),
    };
    const read = new StatusDocsReader({
      root: () => '/fixture',
      now: () => Date.parse('2026-11-08T12:00:00Z'),
      handle: () => handle,
      fs: fixtureFs,
    });
    // Nothing is read; the refusal is named, exactly as the TypeScript head validator would.
    await read.scan();
    const state = read.snapshot();
    expect(state.docs).toEqual([]);
    expect(state.problems).toHaveLength(1);
    expect(state.problems[0]?.reason).toContain('seq is missing or not valid');
  });
});
