import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';

/** Temporary folders made by a test file; `cleanup()` removes them. */
export function tempDirs() {
  const made: string[] = [];
  return {
    make(prefix = 'ccs-status-'): string {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
      made.push(dir);
      return dir;
    },
    cleanup(): void {
      for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
    },
  };
}

export const NOW = Date.parse('2026-11-06T16:30:00.000Z');

export function ago(ms: number, now = NOW): string {
  return new Date(now - ms).toISOString();
}

export function threadDoc(writer: string, fields: Record<string, unknown> = {}) {
  return {
    schema: 'agent-reports/thread@1',
    writer,
    seq: 1,
    writtenAt: ago(60_000),
    ttlSeconds: 1800,
    agent: {
      name: writer.slice(writer.indexOf('.') + 1),
      model: 'claude-opus-5-5',
      effort: 'high',
    },
    project: 'proj',
    stream: 'S',
    state: 'working',
    doing: 'Building',
    blockedOn: null,
    progress: null,
    links: [],
    ...fields,
  };
}

export function tasksDoc(
  writer: string,
  items: Array<Record<string, unknown>>,
  fields: Record<string, unknown> = {}
) {
  return {
    schema: 'agent-reports/tasks@1',
    writer,
    seq: 1,
    writtenAt: ago(60_000),
    project: 'proj',
    projectLabel: 'Project',
    stream: 'S',
    streamLabel: 'Stream S',
    owner: 'lead',
    goal: 'Ship it',
    board: null,
    items,
    ...fields,
  };
}

export function item(id: string, state: string, fields: Record<string, unknown> = {}) {
  return { id, title: `Item ${id}`, state, updatedAt: ago(120_000), ...fields };
}

export function cardDoc(writer: string, local: string, fields: Record<string, unknown> = {}) {
  return {
    schema: 'agent-reports/card@1',
    id: `${writer}--${local}`,
    writer,
    seq: 1,
    writtenAt: ago(60_000),
    project: 'proj',
    stream: 'S',
    askedBy: writer.slice(writer.indexOf('.') + 1),
    replyTo: { pmMail: writer.slice(writer.indexOf('.') + 1), t3Thread: null },
    state: 'open',
    label: `S-${local}`,
    title: `Question ${local}`,
    now: 'Plain words.',
    nowAsOf: null,
    oldAnswer: null,
    whatChanged: null,
    options: [
      { key: 'A', label: 'First', gets: 'Simple', costs: null, image: null },
      { key: 'B', label: 'Second', gets: null, costs: 'More work', image: null },
    ],
    recommendation: { key: 'A', why: 'Simpler.' },
    multiple: false,
    urgency: 'today',
    blocks: null,
    batch: null,
    evidence: [],
    replies: [],
    askedAt: ago(600_000),
    ...fields,
  };
}

/** Writes JSON (or raw bytes) under `<root>/v1/writers/<writer>/`; returns the file's SHA-256. */
export function writeReport(root: string, writer: string, rel: string, value: unknown): string {
  const file = path.join(root, 'v1', 'writers', writer, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const bytes = Buffer.isBuffer(value) ? value : Buffer.from(`${JSON.stringify(value, null, 2)}\n`);
  fs.writeFileSync(file, bytes);
  return createHash('sha256').update(bytes).digest('hex');
}

export function writeCard(root: string, doc: ReturnType<typeof cardDoc>): string {
  return writeReport(root, doc.writer, `cards/${doc.id}.json`, doc);
}
