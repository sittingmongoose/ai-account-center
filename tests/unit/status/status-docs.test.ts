import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../src/status-native/native-loader';
import { StatusDocsReader, type StatusDocsState } from '../../../src/web-server/status/status-docs';

/** A fixture share under a temporary folder. */
function share(files: Record<string, string>): { root: string; cleanup: () => void } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-docs-'));
  for (const [rel, content] of Object.entries(files)) {
    const file = path.join(dir, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, content);
  }
  return { root: dir, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

function statusDoc(writer: string, t3Thread: string, seq: number, extra = {}): string {
  return JSON.stringify({
    schema: 'agent-reports/status@1',
    writer,
    seq,
    writtenAt: '2026-11-08T05:00:00Z',
    t3Thread,
    summary: 'Sweeping.',
    ...extra,
  });
}

/**
 * A fake native handle for `parseStatus`: the same accept/refuse split the module makes
 * (schema, writer, thread, summary), enough for the reader's rules; the document's full
 * validation is the crate's own test suite.
 */
function fakeHandle(options: { refuse?: (writer: string, bytes: Buffer) => string | null } = {}) {
  const calls: string[] = [];
  const handle: NativeStatusHandle = {
    call: async (method, args) => {
      if (method !== 'parseStatus') throw new Error('unexpected method');
      const { writer, contentBase64 } = JSON.parse(args) as {
        writer: string;
        contentBase64: string;
      };
      const bytes = Buffer.from(contentBase64, 'base64');
      calls.push(writer);
      const refusal = options.refuse?.(writer, bytes) ?? null;
      if (refusal) return JSON.stringify({ ok: false, reason: refusal });
      const doc = JSON.parse(bytes.toString('utf8')) as Record<string, unknown>;
      return JSON.stringify({ ok: true, doc, sha256: `sha:${writer}:${doc.seq}` });
    },
  };
  return { handle, calls };
}

function reader(root: string, handle: NativeStatusHandle, now: () => number) {
  return new StatusDocsReader({ root: () => root, now, handle: () => handle });
}

describe('StatusDocsReader', () => {
  it('reads one good status.json per writer', async () => {
    const { root, cleanup } = share({
      'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 3),
      'v1/writers/vm.b/status.json': statusDoc('vm.b', 't2', 7),
      'v1/writers/vm.c/thread.json': '{}', // a writer with no status file
    });
    try {
      const { handle } = fakeHandle();
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      const state: StatusDocsState = read.snapshot();
      expect(state.state).toBe('ok');
      expect(state.docs.map((doc) => doc.writer)).toEqual(['vm.a', 'vm.b']);
      expect(state.docs[0].seq).toBe(3);
      expect(state.docs[0].writtenAtMs).toBe(Date.parse('2026-11-08T05:00:00Z'));
      expect(state.problems).toEqual([]);
    } finally {
      cleanup();
    }
  });

  it('does not read an unchanged file again', async () => {
    const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 1) });
    try {
      const { handle, calls } = fakeHandle();
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      expect(calls).toEqual(['vm.a']);
      await read.scan();
      expect(calls).toEqual(['vm.a']); // same mtime and size: no second parse
    } finally {
      cleanup();
    }
  });

  it('a refused file keeps the last good copy and names the problem', async () => {
    const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 1) });
    try {
      let refuse = false;
      const { handle } = fakeHandle({
        refuse: () => (refuse ? 'summary is missing or not valid' : null),
      });
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      expect(read.snapshot().docs[0].problem).toBeNull();

      // The file changes to something the module refuses.
      refuse = true;
      fs.utimesSync(
        path.join(root, 'v1/writers/vm.a/status.json'),
        new Date(1_794_200_000_000),
        new Date(1_794_200_000_000)
      );
      await read.scan();
      const state = read.snapshot();
      expect(state.docs[0].seq).toBe(1); // the last good copy
      expect(state.docs[0].problem?.reason).toContain('summary is missing or not valid');
      expect(state.problems).toHaveLength(1);
    } finally {
      cleanup();
    }
  });

  it('an older copy is not the document; the higher seq stays', async () => {
    const file = 'v1/writers/vm.a/status.json';
    const { root, cleanup } = share({ [file]: statusDoc('vm.a', 't1', 9) });
    try {
      const { handle } = fakeHandle();
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      // The writer writes a copy with a lower seq (a rolled-back writer).
      fs.writeFileSync(path.join(root, file), statusDoc('vm.a', 't1', 4));
      fs.utimesSync(
        path.join(root, file),
        new Date(1_794_300_000_000),
        new Date(1_794_300_000_000)
      );
      await read.scan();
      const state = read.snapshot();
      expect(state.docs[0].seq).toBe(9);
      expect(state.docs[0].problem).toBeNull();
    } finally {
      cleanup();
    }
  });

  it('a file that leaves the listing is kept through the grace, then dropped', async () => {
    const file = 'v1/writers/vm.a/status.json';
    const { root, cleanup } = share({ [file]: statusDoc('vm.a', 't1', 2) });
    try {
      const { handle } = fakeHandle();
      let now = 1_794_116_000_000;
      const read = reader(root, handle, () => now);
      await read.scan();
      expect(read.snapshot().docs).toHaveLength(1);

      fs.rmSync(path.join(root, file));
      now += 60_000; // inside the 120s grace
      await read.scan();
      expect(read.snapshot().docs).toHaveLength(1);

      now += 130_000; // past it
      await read.scan();
      expect(read.snapshot().docs).toEqual([]);
    } finally {
      cleanup();
    }
  });

  it('a file over the size cap is a problem, never read', async () => {
    const big = statusDoc('vm.a', 't1', 1).replace('Sweeping.', 'Sweeping.'.padEnd(70_000, 'x'));
    const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': big });
    try {
      const { handle, calls } = fakeHandle();
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      expect(calls).toEqual([]); // never handed to the module
      expect(read.snapshot().problems[0]?.reason).toContain('size cap');
    } finally {
      cleanup();
    }
  });

  it('no writers folder is missing, not an error', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-docs-'));
    try {
      const { handle } = fakeHandle();
      const read = reader(path.join(dir, 'empty'), handle, () => 1);
      await read.scan();
      expect(read.snapshot().state).toBe('missing');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('without the module nothing is read and nothing throws', async () => {
    const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 1) });
    try {
      const read = new StatusDocsReader({ root: () => root, now: () => 1, handle: () => null });
      await read.scan();
      expect(read.snapshot().docs).toEqual([]);
    } finally {
      cleanup();
    }
  });
});
