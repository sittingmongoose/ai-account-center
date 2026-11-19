/**
 * G16 producer gate: the readers-check files (`<reports>/v1/readers/aac/<writer>.json`) the TS
 * ReadersCheckWriter writes after one Status scan and one Scoreboard runs scan, for the Rust writer
 * (lane C's aac_status::share::check) to reproduce from the same share at the same clock. The share
 * is a fixture written here and recorded with the outputs; the runs scan is a fixed runs state fed to
 * both writers. Exact bytes.
 *
 *   bun tests/spec/readers-check-vectors.ts          # check tests/spec/readers-check-vectors.json
 *   bun tests/spec/readers-check-vectors.ts --write  # regenerate it (review the diff)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { StatusDocsReader } = await import('../../src/web-server/status/status-docs');
const { ReadersCheckWriter } = await import('../../src/web-server/status/readers-check');
const helpers = await import('../unit/web-server/status/status-test-helpers');
const { NOW, ago, threadDoc, tasksDoc, item, cardDoc, writeReport, writeCard } = helpers;

const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-readers-check-')));

// The share: two writers, a pinned thread, tasks, a good and a refused card, a folder that is no
// writer id (never a check file), and a thread written later than the first one.
writeReport(
  root,
  'vm.lead',
  'thread.json',
  threadDoc('vm.lead', { agent: { ...threadDoc('vm.lead').agent, t3Thread: 'T3-ABC-123' } })
);
writeReport(
  root,
  'vm.lead',
  'tasks.json',
  tasksDoc('vm.lead', [item('t1', 'doing'), item('t2', 'done')])
);
writeCard(root, cardDoc('vm.lead', 'Q1', { seq: 4 }));
writeReport(root, 'vm.lead', 'cards/vm.lead--Q2.json', { ...cardDoc('vm.lead', 'Q2'), title: '' });
writeReport(
  root,
  'mac.worker',
  'thread.json',
  threadDoc('mac.worker', { seq: 7, writtenAt: ago(5_000) })
);
writeReport(root, 'Not A Writer', 'thread.json', threadDoc('vm.other'));

const pins = [
  { id: 't3-thread://host/t3-abc-123', title: 'Lead thread' },
  { id: 'unrelated', title: 'Other' },
];
const runs = {
  runs: [
    {
      file: 'writers/vm.lead/runs/2026-11-06/r1.json',
      doc: { seq: 3 },
      sha256: 'c'.repeat(64),
      firstSeenAt: NOW - 5_000,
    },
    {
      file: 'writers/mac.worker/runs/2026-11-05/a.json',
      doc: { seq: 1 },
      sha256: 'd'.repeat(64),
      firstSeenAt: NOW - 9_000,
    },
  ],
  problems: [
    { file: 'writers/vm.lead/runs/bad.json', reason: 'not valid JSON', since: NOW - 1_000 },
    // Accepted in this same scan: no refused row.
    { file: 'writers/vm.lead/runs/2026-11-06/r1.json', reason: 'stale', since: NOW - 2_000 },
    { file: 'writers/Bad Name/runs/x.json', reason: 'not valid JSON', since: NOW - 3_000 },
    { file: 'writers/mac.worker/thread.json', reason: 'not a runs file', since: NOW - 4_000 },
  ],
};

function walk(dir: string, rel: string, out: Record<string, string>) {
  if (!fs.existsSync(dir)) return;
  for (const name of fs.readdirSync(dir).sort()) {
    const full = path.join(dir, name);
    const relName = rel ? `${rel}/${name}` : name;
    if (fs.statSync(full).isDirectory()) walk(full, relName, out);
    else out[relName] = fs.readFileSync(full, 'utf8');
  }
}
const share: Record<string, string> = {};
walk(path.join(root, 'v1', 'writers'), 'v1/writers', share);

const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
await reader.scan();
const docs = new StatusDocsReader({ root: () => root, now: () => NOW });
const writer = new ReadersCheckWriter({ root: () => root, now: () => NOW });
writer.recordStatusScan(reader.snapshot(), docs.snapshot(), pins as never);
writer.recordRuns(runs as never);
await writer.flush();
const check: Record<string, string> = {};
walk(path.join(root, 'v1', 'readers', 'aac'), 'v1/readers/aac', check);
fs.rmSync(root, { recursive: true, force: true });
assert.ok(Object.keys(check).length >= 2, 'the TS writer wrote check files');

const vectors = { now: NOW, share, pins: pins.map((p) => [p.id, p.title]), runs, check };
const file = path.join(import.meta.dir, 'readers-check-vectors.json');
const bytes = JSON.stringify(vectors, null, 1) + '\n';
if (process.argv.includes('--write')) fs.writeFileSync(file, bytes);
else
  assert.equal(
    fs.readFileSync(file, 'utf8'),
    bytes,
    'readers-check-vectors.json is stale; regenerate and review'
  );
console.log(
  `readers check vectors: ${Object.keys(share).length} share files, ${Object.keys(check).length} check files`
);
