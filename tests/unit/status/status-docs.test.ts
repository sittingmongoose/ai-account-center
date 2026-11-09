import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../src/status-native/native-loader';
import { nodeReaderFs, type ReaderFs } from '../../../src/web-server/status/agent-reports-reader';
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

  // The synthetic tokens of the review probes (2026-11-08/09), joined at runtime so this
  // source file holds none whole. Each is a document the TypeScript check must refuse with
  // its own reason before the native module is ever asked (one secret filter, owner
  // decision 2026-11-09).
  const SECRET_WITNESSES: Array<[string, Record<string, unknown>, string]> = [
    // A bearer token with U+FEFF as its separator: JavaScript's `\s`, not the crate's.
    ['a U+FEFF bearer', { summary: `Bearer\uFEFF${'i'.repeat(24)}` }, 'a bearer token'],
    // A non-ASCII letter beside an API key: the ASCII word boundary still holds.
    ['a non-ASCII API key', { summary: `密sk-${'A'.repeat(24)}` }, 'an API key'],
    // Astral characters count as UTF-16 units to JavaScript's regexes (round-3 M1).
    ['an astral URL value', { summary: `?token=${'😀'.repeat(4)}` }, 'a credential in a URL'],
    [
      'an astral credential value',
      { summary: `x "password":"${'😀'.repeat(3)}" broken` },
      'a credential field',
    ],
    // JavaScript's `trim()` takes U+FEFF off embedded JSON before the walk (round-3 M2).
    [
      'a FEFF-wrapped embedded credential',
      { note: `\uFEFF{\"password\":123456}\uFEFF` },
      'a credential field',
    ],
    // An embedded array deeper than serde's limit still parses in JavaScript (round-3 M3).
    [
      'a deeply embedded credential',
      { note: `${'['.repeat(130)}{"password":123456}${']'.repeat(130)}` },
      'a credential field',
    ],
    // An escaped `?token=` and a lone surrogate escape decode to an 8-unit URL value (M4).
    [
      'an escaped URL with a surrogate',
      { summary: `\\u003f\\u0074\\u006f\\u006b\\u0065\\u006e\\u003d${'a'.repeat(7)}\\ud800` },
      'a credential in a URL',
    ],
  ];

  for (const [name, extra, kind] of SECRET_WITNESSES) {
    it(`${name}: refused by the TypeScript check, never the native module`, async () => {
      const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 1, extra) });
      try {
        const { handle, calls } = fakeHandle();
        const read = reader(root, handle, () => 1_794_116_000_000);
        await read.scan();
        expect(calls).toEqual([]); // the TypeScript check refused it first
        const state = read.snapshot();
        expect(state.docs).toEqual([]); // nothing for the composer to read
        expect(state.problems).toHaveLength(1);
        expect(state.problems[0]?.reason).toBe(`status.json refused: it contains what looks like ${kind}`);
      } finally {
        cleanup();
      }
    });
  }

  it('a clean report with non-ASCII text flows through unchanged', async () => {
    const summary = `清扫 stream F，批次 9 — 😀 done, 收尾 left.`;
    const { root, cleanup } = share({ 'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 2, { summary }) });
    try {
      const { handle } = fakeHandle();
      const read = reader(root, handle, () => 1_794_116_000_000);
      await read.scan();
      const state = read.snapshot();
      expect(state.problems).toEqual([]);
      expect(state.docs).toHaveLength(1);
      expect(state.docs[0].doc.summary).toBe(summary);
      expect(state.docs[0].problem).toBeNull();
    } finally {
      cleanup();
    }
  });

  it('a share root that cannot be listed marks its cached documents unreadable', async () => {
    const file = 'v1/writers/vm.a/status.json';
    const { root, cleanup } = share({ [file]: statusDoc('vm.a', 't1', 5) });
    try {
      const { handle } = fakeHandle();
      let fail = false;
      const failingFs: ReaderFs = {
        readdir: async (dir) => {
          // An EIO on the writers root: an ordinary read failure, not the share's race.
          if (fail && dir === path.join(root, 'v1/writers')) {
            throw Object.assign(new Error('fixture I/O failure'), { code: 'EIO' });
          }
          return fs.promises.readdir(dir, { withFileTypes: true });
        },
        stat: (at) => nodeReaderFs.stat(at),
        readFile: (at, cap) => nodeReaderFs.readFile(at, cap),
      };
      let now = 1_794_116_000_000;
      const read = new StatusDocsReader({
        root: () => root,
        now: () => now,
        fs: failingFs,
        handle: () => handle,
      });
      await read.scan();
      expect(read.snapshot().docs[0].problem).toBeNull();

      fail = true;
      now += 10_000;
      await read.scan();
      let state: StatusDocsState = read.snapshot();
      expect(state.state).toBe('error');
      // The last good copy stays, marked unreadable since the failure began.
      expect(state.docs[0].seq).toBe(5);
      expect(state.docs[0].problem?.reason).toContain('folder could not be listed');
      const since = state.docs[0].problem?.since;
      expect(since).toBe(now);

      now += 10_000;
      await read.scan(); // the failure repeats: the first-failure clock is stable
      state = read.snapshot();
      expect(state.docs[0].problem?.since).toBe(since);
      expect(state.problems).toHaveLength(1);

      fail = false;
      now += 10_000;
      await read.scan(); // the share lists again: the document is re-read and clears
      state = read.snapshot();
      expect(state.state).toBe('ok');
      expect(state.docs[0].problem).toBeNull();
      expect(state.problems).toEqual([]);
    } finally {
      cleanup();
    }
  });

  it('one transient miss on the root is the share race; the second names it from the first', async () => {
    const file = 'v1/writers/vm.a/status.json';
    const { root, cleanup } = share({ [file]: statusDoc('vm.a', 't1', 5) });
    try {
      const { handle } = fakeHandle();
      let stale = false;
      const staleFs: ReaderFs = {
        readdir: async (dir) => {
          if (stale && dir === path.join(root, 'v1/writers')) {
            throw Object.assign(new Error('stale file handle'), { code: 'ESTALE' });
          }
          return fs.promises.readdir(dir, { withFileTypes: true });
        },
        stat: (at) => nodeReaderFs.stat(at),
        readFile: (at, cap) => nodeReaderFs.readFile(at, cap),
      };
      let now = 1_794_116_000_000;
      const read = new StatusDocsReader({
        root: () => root,
        now: () => now,
        fs: staleFs,
        handle: () => handle,
      });
      await read.scan();

      stale = true;
      const firstMiss = now + 10_000;
      now = firstMiss;
      await read.scan(); // the share's rename-over race: quiet, nothing is marked
      expect(read.snapshot().docs[0].problem).toBeNull();

      now += 10_000;
      await read.scan(); // the same miss again: named, since the first miss
      const state = read.snapshot();
      expect(state.docs[0].problem?.reason).toContain('two scans in a row');
      expect(state.docs[0].problem?.since).toBe(firstMiss);
    } finally {
      cleanup();
    }
  });

  it('a writer folder that cannot be listed marks its document, not the others', async () => {
    const { root, cleanup } = share({
      'v1/writers/vm.a/status.json': statusDoc('vm.a', 't1', 3),
      'v1/writers/vm.b/status.json': statusDoc('vm.b', 't2', 4),
    });
    try {
      const { handle } = fakeHandle();
      let failB: NodeJS.ErrnoException['code'] | null = null;
      const failingFs: ReaderFs = {
        readdir: async (dir) => {
          if (failB && dir === path.join(root, 'v1/writers/vm.b')) {
            throw Object.assign(new Error('fixture failure'), { code: failB });
          }
          return fs.promises.readdir(dir, { withFileTypes: true });
        },
        stat: (at) => nodeReaderFs.stat(at),
        readFile: (at, cap) => nodeReaderFs.readFile(at, cap),
      };
      let now = 1_794_116_000_000;
      const read = new StatusDocsReader({
        root: () => root,
        now: () => now,
        fs: failingFs,
        handle: () => handle,
      });
      await read.scan();

      failB = 'EIO';
      now += 10_000;
      await read.scan(); // an ordinary read failure is named at once
      let state = read.snapshot();
      expect(state.state).toBe('ok');
      expect(state.docs[0].problem).toBeNull(); // vm.a is untouched
      expect(state.docs[1].problem?.reason).toContain('folder could not be listed');
      expect(state.docs[1].problem?.since).toBe(now);

      failB = 'ESTALE';
      const firstMiss = now + 10_000;
      now = firstMiss;
      await read.scan(); // the failure turns transient: quiet for one scan, its first miss
      state = read.snapshot();
      expect(state.docs[1].problem?.reason).toContain('folder could not be listed');

      now += 10_000;
      await read.scan(); // the second transient miss in a row: named, since the first of them
      state = read.snapshot();
      expect(state.docs[1].problem?.reason).toContain('two scans in a row');
      expect(state.docs[1].problem?.since).toBe(firstMiss);
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
