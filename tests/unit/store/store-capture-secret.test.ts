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
          await expect(store.captureReports([report])).rejects.toThrow(
            'capture report failed validation'
          );
          expect(await store.captureHistory(query)).toEqual(before);
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
