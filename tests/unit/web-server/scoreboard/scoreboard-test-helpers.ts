import fs from 'fs';
import path from 'path';
import { NOW } from '../status/status-test-helpers';

export function runDoc(writer: string, local: string, fields: Record<string, unknown> = {}) {
  const ended = (fields.endedAt as string | undefined) ?? new Date(NOW - 3_600_000).toISOString();
  return {
    schema: 'agent-reports/run@1',
    id: `${writer}--${local}`,
    writer,
    seq: 1,
    writtenAt: ended,
    source: 'observed',
    project: 'proj',
    stream: null,
    task: { id: local, type: 'build', title: `Task ${local}`, size: { tasks: 1 } },
    model: { provider: 'anthropic', id: 'claude-opus-5-5', harness: 'claude-code', effort: 'high' },
    startedAt: null,
    endedAt: ended,
    wallSeconds: 100,
    outcome: 'done',
    ...fields,
  };
}

export function writeRun(root: string, doc: ReturnType<typeof runDoc>, date = '2026-11-06') {
  const dir = path.join(root, 'v1', 'writers', doc.writer, 'runs', date);
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, `${doc.id}.json`);
  fs.writeFileSync(file, JSON.stringify(doc));
  return file;
}
