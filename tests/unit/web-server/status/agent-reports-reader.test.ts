import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  AgentReportsReader,
  ensureReportsLayout,
  isTransientShareError,
  MISSING_GRACE_MS,
  PERSISTENT_LISTING_ERROR,
  shareLocalPath,
} from '../../../../src/web-server/status/agent-reports-reader';
import {
  cardDoc,
  item,
  NOW,
  tasksDoc,
  tempDirs,
  threadDoc,
  writeCard,
  writeReport,
} from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

function reader(root: string, now = () => NOW) {
  return new AgentReportsReader({ root: () => root, now });
}

describe('agent reports reader', () => {
  it('reports a missing folder, then reads valid files and ignores dot files', async () => {
    const root = dirs.make();
    const r = reader(root);
    await r.scan();
    expect(r.snapshot().state).toBe('missing');
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead'));
    writeReport(root, 'vm.lead', 'tasks.json', tasksDoc('vm.lead', [item('A', 'todo')]));
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    writeReport(root, 'vm.lead', 'cards/.vm.lead--Q2.json.tmp-abc', { half: true });
    writeReport(root, '.hidden', 'thread.json', {});
    expect(await r.scan()).toBe(true);
    const state = r.snapshot();
    expect(state.state).toBe('ok');
    expect(state.threads).toHaveLength(1);
    expect(state.tasks).toHaveLength(1);
    expect(state.cards.map((c) => c.doc.id)).toEqual(['vm.lead--Q1']);
    expect(state.problems).toEqual([]);
    expect(await r.scan()).toBe(false);
  });

  it('keeps the last good copy of a file that turns malformed or gets NUL bytes, and names it', async () => {
    const root = dirs.make();
    let now = NOW;
    const r = reader(root, () => now);
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { doing: 'First' }));
    await r.scan();
    now += 10_000;
    writeReport(root, 'vm.lead', 'thread.json', Buffer.from('{"schema": "agent-'));
    expect(await r.scan()).toBe(true);
    let state = r.snapshot();
    expect(state.threads[0].doc.doing).toBe('First');
    expect(state.threads[0].problem).toEqual({
      file: 'writers/vm.lead/thread.json',
      reason: 'not valid JSON',
      since: now,
    });
    now += 10_000;
    writeReport(
      root,
      'vm.lead',
      'thread.json',
      Buffer.concat([Buffer.from(JSON.stringify(threadDoc('vm.lead'))), Buffer.alloc(16)])
    );
    await r.scan();
    state = r.snapshot();
    expect(state.problems.map((p) => p.reason)).toEqual([
      'contains NUL bytes from a damaged write',
    ]);
    expect(state.threads[0].doc.doing).toBe('First');
    // A file with no good copy is still listed by name.
    writeReport(root, 'mac.other', 'tasks.json', Buffer.from('not json'));
    await r.scan();
    expect(r.snapshot().problems.map((p) => p.file)).toEqual([
      'writers/mac.other/tasks.json',
      'writers/vm.lead/thread.json',
    ]);
    // Fixed: the problem clears.
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 2, doing: 'Second' }));
    await r.scan();
    expect(r.snapshot().threads[0].doc.doing).toBe('Second');
    expect(r.snapshot().threads[0].problem).toBeNull();
  });

  it('ignores a lower seq than the one already accepted, and says so', async () => {
    const root = dirs.make();
    const r = reader(root);
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 5, doing: 'Newer' }));
    await r.scan();
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 4, doing: 'Older!' }));
    await r.scan();
    expect(r.snapshot().threads[0].doc.doing).toBe('Newer');
    expect(r.snapshot().problems.map((p) => p.file)).toEqual(['writers/vm.lead/thread.json']);
    writeReport(
      root,
      'vm.lead',
      'thread.json',
      threadDoc('vm.lead', { seq: 6, doing: 'The newest' })
    );
    await r.scan();
    expect(r.snapshot().threads[0].doc.doing).toBe('The newest');
    expect(r.snapshot().problems).toEqual([]);
  });

  it('records when it first saw a version, so a clock running ahead cannot look fresh', async () => {
    const root = dirs.make();
    const r = reader(root);
    writeReport(
      root,
      'vm.lead',
      'thread.json',
      threadDoc('vm.lead', { writtenAt: new Date(NOW + 3_600_000).toISOString() })
    );
    await r.scan();
    const seen = r.snapshot().threads[0];
    expect(seen.firstSeenAt).toBe(NOW);
    expect(seen.writtenAtMs).toBe(NOW + 3_600_000);
  });

  it('drops withdrawn files, refuses bad writer folders and stats boards inside the share', async () => {
    const root = path.join(dirs.make(), 'agent-reports');
    const boardFile = path.join(root, 'boards', 'S.md');
    fs.mkdirSync(path.dirname(boardFile), { recursive: true });
    fs.writeFileSync(boardFile, '# S');
    writeReport(
      root,
      'vm.lead',
      'thread.json',
      threadDoc('vm.lead', { links: [{ label: 'Board', path: boardFile }] })
    );
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    writeReport(root, 'Laptop.x', 'thread.json', {});
    let now = NOW;
    const r = reader(root, () => now);
    await r.scan();
    expect(r.snapshot().boards.get(boardFile)).toBeGreaterThan(0);
    expect(r.snapshot().problems.map((p) => p.file)).toEqual(['writers/Laptop.x']);
    fs.rmSync(path.join(root, 'v1', 'writers', 'vm.lead', 'cards', 'vm.lead--Q1.json'));
    fs.rmSync(path.join(root, 'v1', 'writers', 'vm.lead', 'thread.json'));
    // A rename-over is a delete then a rename: the last good copies stay through the grace.
    expect(await r.scan()).toBe(false);
    now += MISSING_GRACE_MS - 1000;
    expect(await r.scan()).toBe(false);
    expect(r.snapshot().cards).toHaveLength(1);
    expect(r.snapshot().threads).toHaveLength(1);
    now += 2000;
    expect(await r.scan()).toBe(true);
    expect(r.snapshot().cards).toEqual([]);
    expect(r.snapshot().threads).toEqual([]);
  });

  it('keeps the last good copy through ENOENT and ESTALE reads and retries next scan', async () => {
    const root = dirs.make();
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { doing: 'First' }));
    let fail: 'ENOENT' | 'ESTALE' | null = null;
    const r = new AgentReportsReader({
      root: () => root,
      now: () => NOW,
      fs: {
        readdir: (dir) => fs.promises.readdir(dir, { withFileTypes: true }),
        stat: (file) => fs.promises.stat(file),
        readFile: async (file) => {
          if (fail === 'ESTALE')
            throw Object.assign(new Error('Unknown system error -116'), {
              errno: -116,
              code: 'Unknown system error -116',
            });
          if (fail === 'ENOENT') throw Object.assign(new Error('gone'), { code: 'ENOENT' });
          return fs.promises.readFile(file);
        },
      },
    });
    await r.scan();
    for (const kind of ['ESTALE', 'ENOENT'] as const) {
      writeReport(
        root,
        'vm.lead',
        'thread.json',
        threadDoc('vm.lead', { seq: 2, doing: `During ${kind}` })
      );
      fail = kind;
      await r.scan();
      expect(r.snapshot().threads[0].doc.doing).not.toBe(`During ${kind}`);
      expect(r.snapshot().problems).toEqual([]);
      fail = null;
      await r.scan();
      expect(r.snapshot().threads[0].doc.doing).toBe(`During ${kind}`);
    }
    expect(isTransientShareError({ code: 'ESTALE' })).toBe(true);
    expect(isTransientShareError({ code: 'EACCES' })).toBe(false);
  });

  it('names a listed report whose ENOENT or ESTALE persists past one scan', async () => {
    for (const code of ['ENOENT', 'ESTALE'] as const) {
      const root = dirs.make();
      writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { doing: 'First' }));
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      let failing = false;
      let now = NOW;
      const r = new AgentReportsReader({
        root: () => root,
        now: () => now,
        fs: {
          readdir: (dir) => fs.promises.readdir(dir, { withFileTypes: true }),
          stat: async (file) => {
            if (failing && file.endsWith('thread.json'))
              throw Object.assign(new Error(code), { code });
            return fs.promises.stat(file);
          },
          readFile: (file) => fs.promises.readFile(file),
        },
      });
      await r.scan();
      failing = true;
      now += 60_000;
      await r.scan();
      expect(r.snapshot().problems).toEqual([]);
      now += 60_000;
      await r.scan();
      const state = r.snapshot();
      expect(state.state).toBe('ok');
      expect(state.threads[0].doc.doing).toBe('First');
      expect(state.problems).toEqual([
        {
          file: 'writers/vm.lead/thread.json',
          reason: expect.stringContaining('two scans in a row'),
          since: NOW + 60_000,
        },
      ]);
      expect(state.threads[0].problem?.file).toBe('writers/vm.lead/thread.json');
      expect(state.complete).toBe(false);
      failing = false;
      await r.scan();
      expect(r.snapshot().problems).toEqual([]);
      expect(r.snapshot().threads[0].problem).toBeNull();
      expect(r.snapshot().complete).toBe(true);
    }
  });

  it('names a writer or cards folder whose listing fails on two scans in a row, on its rows too', async () => {
    for (const [folder, code] of [
      ['vm.lead', 'ESTALE'],
      ['vm.lead', 'ENOENT'],
      ['cards', 'ESTALE'],
      ['vm.lead', 'EACCES'],
    ] as const) {
      const root = dirs.make();
      writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { doing: 'First' }));
      writeCard(root, cardDoc('vm.lead', 'Q1'));
      let failing = false;
      let now = NOW;
      const r = new AgentReportsReader({
        root: () => root,
        now: () => now,
        fs: {
          readdir: (dir) => {
            if (failing && path.basename(dir) === folder)
              throw Object.assign(new Error(code), { code });
            return fs.promises.readdir(dir, { withFileTypes: true });
          },
          stat: (file) => fs.promises.stat(file),
          readFile: (file) => fs.promises.readFile(file),
        },
      });
      await r.scan();
      failing = true;
      now += 60_000;
      await r.scan();
      const rel = folder === 'cards' ? 'writers/vm.lead/cards' : 'writers/vm.lead';
      if (code !== 'EACCES') {
        // Once is the share's race: nothing said, every row kept.
        expect(r.snapshot().problems).toEqual([]);
        expect(r.snapshot().threads[0].problem).toBeNull();
        expect(r.snapshot().complete).toBe(false);
        now += 60_000;
        await r.scan();
      }
      const state = r.snapshot();
      // Since the first failure, not the scan that named it.
      const since = NOW + 60_000;
      const reason = code === 'EACCES' ? 'folder could not be listed' : PERSISTENT_LISTING_ERROR;
      expect(state.state).toBe('ok');
      expect(state.problems).toContainEqual({ file: rel, reason, since });
      // Each row under the folder is its last good reading, with the folder's problem.
      const card = state.cards[0];
      expect(card?.problem?.reason).toBe(`its ${reason}`);
      expect(card?.problem?.since).toBe(since);
      if (folder === 'cards') expect(state.threads[0].problem).toBeNull();
      else {
        expect(state.threads[0].doc.doing).toBe('First');
        expect(state.threads[0].problem?.reason).toBe(`its ${reason}`);
      }
      expect(state.complete).toBe(false);
      // A third failing scan keeps the first failure's time.
      now += 60_000;
      await r.scan();
      expect(r.snapshot().problems).toContainEqual({ file: rel, reason, since });
      failing = false;
      await r.scan();
      expect(r.snapshot().problems).toEqual([]);
      expect(r.snapshot().threads[0].problem).toBeNull();
      expect(r.snapshot().cards[0].problem).toBeNull();
      expect(r.snapshot().complete).toBe(true);
      // After a good listing, one failure is the race again.
      failing = true;
      if (code !== 'EACCES') {
        await r.scan();
        expect(r.snapshot().problems).toEqual([]);
      }
    }
  });

  it('accepts T3 writers without a pm-mail name, and quiet threads without a heartbeat', async () => {
    const root = dirs.make();
    writeReport(
      root,
      'mac.t3-1a2b3c4d',
      'thread.json',
      threadDoc('mac.t3-1a2b3c4d', { ttlSeconds: null, state: 'idle' })
    );
    writeReport(
      root,
      'vm.busy',
      'thread.json',
      threadDoc('vm.busy', { ttlSeconds: null, state: 'working' })
    );
    const r = reader(root);
    await r.scan();
    expect(r.snapshot().threads.map((t) => t.writer)).toEqual(['mac.t3-1a2b3c4d']);
    expect(r.snapshot().threads[0].doc.ttlSeconds).toBeNull();
    expect(r.snapshot().problems.map((p) => p.reason)).toEqual([
      'ttlSeconds is missing or not valid',
    ]);
  });

  it('creates v1 and its folders under an existing root only', async () => {
    const root = dirs.make();
    const created = await ensureReportsLayout(root);
    expect(created.sort()).toEqual(
      ['v1', 'v1/answers', 'v1/catalog', 'v1/keys', 'v1/schemas', 'v1/writers'].sort()
    );
    expect(fs.statSync(path.join(root, 'v1', 'writers')).mode & 0o777).toBe(0o777);
    expect(await ensureReportsLayout(root)).toEqual([]);
    const unmounted = path.join(dirs.make(), 'agent-reports');
    expect(await ensureReportsLayout(unmounted)).toEqual([]);
    expect(fs.existsSync(unmounted)).toBe(false);
  });

  it('reads a card fresh for an answer', async () => {
    const root = dirs.make();
    const r = reader(root);
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    expect((await r.readCardNow('vm.lead--Q1'))?.sha256).toBe(sha);
    expect(await r.readCardNow('vm.lead--Q9')).toBeNull();
    expect(await r.readCardNow('../../etc--x')).toBeNull();
  });

  it('maps share paths from every machine onto this computer and nothing outside it', () => {
    const root = '/mnt/Cursor/agent-reports';
    expect(shareLocalPath('/Volumes/Cursor/share/a/F.md', root)).toBe('/mnt/Cursor/share/a/F.md');
    expect(shareLocalPath('\\\\TRUENAS\\Cursor\\share\\a\\F.md', root)).toBe(
      '/mnt/Cursor/share/a/F.md'
    );
    expect(shareLocalPath('/mnt/Cursor/share/../../../etc/passwd', root)).toBeNull();
    expect(shareLocalPath('/etc/passwd', root)).toBeNull();
    expect(shareLocalPath('relative/F.md', root)).toBeNull();
  });
});
