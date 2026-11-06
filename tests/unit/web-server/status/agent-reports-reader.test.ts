import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  AgentReportsReader,
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
    expect(state.problems.map((p) => p.reason)).toEqual(['contains NUL bytes from a damaged write']);
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

  it('ignores a lower seq than the one already accepted', async () => {
    const root = dirs.make();
    const r = reader(root);
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 5, doing: 'Newer' }));
    await r.scan();
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 4, doing: 'Older!' }));
    await r.scan();
    expect(r.snapshot().threads[0].doc.doing).toBe('Newer');
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
    const r = reader(root);
    await r.scan();
    expect(r.snapshot().boards.get(boardFile)).toBeGreaterThan(0);
    expect(r.snapshot().problems.map((p) => p.file)).toEqual(['writers/Laptop.x']);
    fs.rmSync(path.join(root, 'v1', 'writers', 'vm.lead', 'cards', 'vm.lead--Q1.json'));
    expect(await r.scan()).toBe(true);
    expect(r.snapshot().cards).toEqual([]);
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
    expect(shareLocalPath('\\\\TRUENAS\\Cursor\\share\\a\\F.md', root)).toBe('/mnt/Cursor/share/a/F.md');
    expect(shareLocalPath('/mnt/Cursor/share/../../../etc/passwd', root)).toBeNull();
    expect(shareLocalPath('/etc/passwd', root)).toBeNull();
    expect(shareLocalPath('relative/F.md', root)).toBeNull();
  });
});
