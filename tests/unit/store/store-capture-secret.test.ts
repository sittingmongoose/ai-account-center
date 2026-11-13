import { expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import cases from '../../../native/aac-store/tests/report-secret-cases.json';
import { loadNativeStore, nativeModuleName } from '../../../src/store/native-loader';
import { AacStore, type StoreReportCapture } from '../../../src/store/store-service';

const loaded = loadNativeStore({
  env: {},
  file: path.resolve(__dirname, '../../../dist/native', nativeModuleName()),
});

(loaded.module ? it : it.skip)(
  'native API refuses reader secret fixtures without appending or retaining their content',
  async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-capture-secrets-'));
    const handle = new loaded.module!.NativeStore();
    try {
      await handle.call('open', JSON.stringify({ dir: root }));
      const store = new AacStore(handle, root);
      const query = { kind: 'thread', writer: 'vm.example', objectId: 'vm.example' };
      const ledger = () =>
        fs
          .readdirSync(path.join(root, 'log', 'ledger'))
          .sort()
          .flatMap((name) => {
            const file = path.join(root, 'log', 'ledger', name);
            return fs.lstatSync(file).isFile()
              ? [[name, createHash('sha256').update(fs.readFileSync(file)).digest('hex')]]
              : [];
          });
      let seq = 0;
      for (const fixture of cases) {
        const doc = {
          schema: 'agent-reports/thread@1',
          writer: query.writer,
          seq: ++seq,
          ...fixture.doc,
        };
        const report: StoreReportCapture = {
          ...query,
          kind: 'thread',
          seq,
          writtenAtMs: 1_794_564_000_000 + seq,
          observedAtMs: 1_794_564_001_000 + seq,
          sourceSha256: createHash('sha256').update(JSON.stringify(doc)).digest('hex'),
          ids: {},
          doc,
        };
        const before = await store.captureHistory(query);
        if (fixture.secret) {
          const beforeLedger = ledger();
          const beforeCapture = (await store.status()).capture;
          const validDoc = {
            schema: doc.schema,
            writer: doc.writer,
            seq: seq + 10_000,
            doing: 'Accepted metadata',
          };
          const valid = {
            ...report,
            seq: validDoc.seq,
            doc: validDoc,
            sourceSha256: createHash('sha256').update(JSON.stringify(validDoc)).digest('hex'),
          };
          await expect(store.captureReports([valid, report])).rejects.toThrow(
            'capture report failed validation'
          );
          expect(await store.captureHistory(query)).toEqual(before);
          expect((await store.status()).capture).toEqual(beforeCapture);
          expect(ledger()).toEqual(beforeLedger);
        } else {
          expect(await store.captureReports([report])).toEqual({ stored: 1, duplicates: 0 });
        }
      }
    } finally {
      await handle.call('close', '{}');
      fs.rmSync(root, { recursive: true, force: true });
    }
  }
);

(loaded.module ? it : it.skip)(
  'reports quota temporary count/bytes without deleting live or old files',
  async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-quota-temp-health-'));
    const directory = path.join(root, 'account-analytics');
    fs.mkdirSync(directory);
    const live = path.join(
      directory,
      'quota-history-v1.json.00000000-0000-4000-8000-000000000000.tmp'
    );
    const old = path.join(
      directory,
      'quota-history-v1.json.00000000-0000-4000-8000-000000000001.tmp'
    );
    const main = path.join(directory, 'quota-history-v1.json');
    fs.writeFileSync(main, 'main sentinel');
    fs.writeFileSync(old, 'old');
    fs.writeFileSync(path.join(directory, 'unrelated.tmp'), 'unrelated');
    const fd = fs.openSync(live, 'wx', 0o600);
    fs.writeFileSync(fd, 'pending payload');
    const suspended = new Date(Date.now() - 25 * 3_600_000);
    fs.futimesSync(fd, suspended, suspended);
    const originalMtime = fs.fstatSync(fd).mtimeMs;
    let descriptorOpen = true;
    const handle = new loaded.module!.NativeStore();
    try {
      await handle.call('open', JSON.stringify({ dir: path.join(root, 'store') }));
      const health = (
        JSON.parse(await handle.call('status', '{}')) as {
          storageHealth: { quotaTemporaries: Record<string, unknown> };
        }
      ).storageHealth.quotaTemporaries;
      expect(health).toMatchObject({
        count: 2,
        bytes: 18,
        scan: 'complete',
        automaticDeletion: false,
      });
      expect(health.diagnostic).toContain('stop every writer');
      expect(fs.readFileSync(main, 'utf8')).toBe('main sentinel');
      expect(fs.readFileSync(old, 'utf8')).toBe('old');
      expect(fs.fstatSync(fd).mtimeMs).toBe(originalMtime);
      fs.closeSync(fd);
      descriptorOpen = false;
      fs.renameSync(live, path.join(directory, 'published.json'));
      expect(fs.readFileSync(path.join(directory, 'published.json'), 'utf8')).toBe(
        'pending payload'
      );
    } finally {
      if (descriptorOpen) fs.closeSync(fd);
      await handle.call('close', '{}');
      fs.rmSync(root, { recursive: true, force: true });
    }
  }
);
