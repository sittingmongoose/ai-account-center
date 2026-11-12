import { describe, expect, it } from 'bun:test';
import { Database } from 'bun:sqlite';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { nativeStatusModuleName } from '../../../../src/status-native/native-loader';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { PinnedThreadsCollector } from '../../../../src/web-server/status/pinned-threads';
import { buildStatusView } from '../../../../src/web-server/status/status-view';
import {
  ago,
  cardDoc,
  item,
  NOW,
  tasksDoc,
  threadDoc,
  writeCard,
  writeReport,
} from './status-test-helpers';

/**
 * The end-to-end fixture (D14): real-shaped report files and a fixture T3 pin database,
 * read by the real readers and the real composer to the /api/status JSON. The view is
 * never hand-written: the headline, who line, looping and waiting outputs are whatever
 * the chain composes from the files on disk.
 */

/** The module `bun run status:build` leaves behind; the end-to-end path needs the real one. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
// With AAC_REQUIRE_NATIVE a missing module fails instead of skipping, so a host without
// a build cannot stay green by never running this (F12). CI is not required: the CI
// job never builds the module, so forcing it there would go red on every push (N2).
const required = Boolean(process.env.AAC_REQUIRE_NATIVE);
const withModule = hasModule || required ? it : it.skip;

const PIN_ID = 'pin-1';
const PIN_TITLE = 'Build PMConcept7';

/** A fixture T3 state database in a temporary folder. Never the real `~/.t3`. */
function fixturePinDb(): { file: string; cleanup: () => void } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-e2e-pins-'));
  const file = path.join(dir, 'statev2.sqlite');
  const db = new Database(file, { create: true });
  db.exec(`CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL,
    default_provider TEXT NOT NULL, runtime_mode TEXT NOT NULL, interaction_mode TEXT NOT NULL,
    active_provider_thread_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    archived_at TEXT, deleted_at TEXT, payload_json TEXT NOT NULL, provider_instance_id TEXT)`);
  db.prepare(
    `INSERT INTO orchestration_v2_projection_threads
     (thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
      created_at, updated_at, archived_at, deleted_at, payload_json)
     VALUES (?, 'proj', ?, 'claude', 'auto', 'default', '2026-11-01T00:00:00Z',
             '2026-11-02T00:00:00Z', null, null, ?)`
  ).run(
    PIN_ID,
    PIN_TITLE,
    JSON.stringify({
      id: PIN_ID,
      title: PIN_TITLE,
      pinnedAt: '2026-11-06T10:00:00Z',
      pinOrderKey: 'a',
      lineage: { parentThreadId: null },
    })
  );
  db.close();
  return { file, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

describe('the status endpoint from real files', () => {
  withModule('reads report files and pins through the real readers and composer', async () => {
    const reports = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-e2e-reports-'));
    const db = fixturePinDb();
    try {
      // The pinned thread's own reports: thread@1 (raw model id), its stream's tasks@1
      // (labels plus three failing reviews), and its status@1 (waiting on the owner).
      writeReport(
        reports,
        'vm.lead',
        'thread.json',
        threadDoc('vm.lead', {
          seq: 5,
          project: 'puppet-master',
          stream: 'F',
          doing: 'Windows 10 sweep for stream F.',
          agent: {
            name: 'lead',
            harness: 'claude-code',
            model: 'claude-opus-5-5',
            effort: 'xhigh',
            account: 'claude_main',
            machine: 'vm',
            t3Thread: PIN_ID,
            parent: null,
          },
        })
      );
      writeReport(
        reports,
        'vm.lead',
        'tasks.json',
        tasksDoc(
          'vm.lead',
          [
            item('F-07', 'fixing', {
              title: 'Tray icon sharp at 125% scale',
              reviews: [
                { by: 'reviewer', verdict: 'FAIL, one Medium', at: ago(3 * 3600_000) },
                { by: 'reviewer', verdict: 'FAIL, one Medium', at: ago(2 * 3600_000) },
                { by: 'reviewer', verdict: 'FAIL, one Medium', at: ago(3600_000) },
              ],
            }),
            item('F-08', 'landed'),
          ],
          {
            project: 'puppet-master',
            projectLabel: 'Puppet Master',
            stream: 'F',
            streamLabel: 'PM7 follow-ups',
          }
        )
      );
      writeReport(reports, 'vm.lead', 'status.json', {
        schema: 'agent-reports/status@1',
        writer: 'vm.lead',
        seq: 7,
        writtenAt: ago(120_000),
        t3Thread: PIN_ID,
        summary: 'Windows 10 sweep for stream F is on batch 9.',
        percent: null,
        done: [{ id: 'F-08', title: 'Sticker block landed' }],
        left: [{ id: 'F-07', title: 'Tray icon sharp at 125% scale' }],
        blockers: [{ text: 'Pick the reviewers for batch B09', waitingOn: 'owner' }],
        looping: null,
      });
      // Headline cards from another writer: one question, one objection, one sample.
      writeCard(reports, cardDoc('vm.other', 'Q1'));
      writeCard(
        reports,
        cardDoc('vm.other', 'O1', { kind: 'objection', options: [], recommendation: null })
      );
      writeCard(
        reports,
        cardDoc('vm.other', 'S1', { kind: 'sample', options: [], recommendation: null })
      );

      const reader = new AgentReportsReader({ root: () => reports, now: () => NOW });
      await reader.scan();
      const state = reader.snapshot();
      expect(state.problems).toEqual([]);

      const collector = new PinnedThreadsCollector({
        file: PREBUILT,
        dbPath: db.file,
        readRemote: async () => '[]',
        now: () => NOW,
        timeZone: () => 'America/New_York',
        reportsRoot: () => reports,
      });
      await collector.collect();
      await collector.scanStatusDocs();
      await collector.compose(state, []);
      const pinned = collector.view();
      if ('unavailable' in pinned) throw new Error(`panel unavailable: ${pinned.unavailable}`);
      expect(pinned.updates).toHaveLength(1);
      const update = pinned.updates[0];
      // The who line is the project label, the thread name and the catalog model label.
      expect(update.who).toBe('Puppet Master · lead · Opus 5.5 xhigh');
      // The board's failing reviews loop, and the status document waits on the owner.
      expect(update.looping.answer).toBe('yes');
      expect(update.looping.task).toBe('F-07');
      expect(update.blocked?.waitingOnYou).toBe(true);
      expect(update.blocked?.text).toContain('Pick the reviewers for batch B09');

      const empty = { data: null, asOf: null, error: null };
      const view = buildStatusView({
        now: NOW,
        timeZone: 'America/New_York',
        reports: state,
        registry: { data: [], asOf: NOW, error: null },
        machines: { vm: empty, mac: empty, windows: empty, nas1: empty, alexis: empty },
        browsers: empty,
        simlock: empty,
        usage: null,
        answers: [],
        pinned,
      });
      // The /api/status headline names the looping and waiting pins, counts the question
      // and the decision, and never counts the sample.
      const line = view.headline.runs.map((run) => run.text).join('');
      expect(line).toContain('1 question for you');
      expect(line).toContain('1 decision to look at');
      expect(line).toContain(`1 pinned thread is looping (${PIN_TITLE}, task F-07)`);
      expect(line).toContain(`1 pinned thread waits on you (${PIN_TITLE})`);
      expect(view.headline.counts).toMatchObject({
        openCards: 3,
        questions: 1,
        decisions: 1,
        loopingPinned: 1,
        waitingPinned: 1,
      });
      expect(view.headline.tone).toBe('warn');
    } finally {
      db.cleanup();
      fs.rmSync(reports, { recursive: true, force: true });
    }
  });
});
