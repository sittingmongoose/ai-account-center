import { describe, expect, it } from 'bun:test';
import { existsSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  haveNativeStoreServer,
  startNativeStoreServer,
} from '../../../support/native-store-server';

const worker = process.env.AAC_STATUS_TEST_WORKER;
describe.skipIf(!haveNativeStoreServer)('Status with the history store off', () => {
  const switches = [
    ['AAC_STORE=off', 'off', {}],
    ['AAC_RUST_STORE=off', 'on', { rustStore: 'off' }],
    ['rustService.store: off', 'on', { configStore: 'off' }],
  ] as const;

  for (const [label, store, settings] of switches) {
    it(
      label,
      async () => {
        const root = await mkdtemp(path.join(os.tmpdir(), 'aac-status-store-off-'));
        let fixture: Awaited<ReturnType<typeof startNativeStoreServer>> | undefined;
        try {
          fixture = await startNativeStoreServer(root, store, { ...settings, worker });
          const { send, cookie, url } = fixture;
          const headers = { cookie, origin: url };
          for (const [route, view] of [
            ['/api/status', 'status@1'],
            ['/api/scoreboard', 'scoreboard@1'],
          ]) {
            const response = await send(route, { headers });
            expect([route, response.status]).toEqual([route, 200]);
            expect(response.headers.get('x-aac-view')).toBe(view);
            if (route.endsWith('scoreboard'))
              expect((await response.json()).history.store).toBe('off');
            else await response.arrayBuffer();
          }
          const history = await send('/api/status/answers', { headers });
          expect(history.status).toBe(200);
          expect(await history.json()).toEqual({ answers: [] });
          const post = await send('/api/status/answers', {
            method: 'POST',
            headers: { ...headers, 'content-type': 'application/json' },
            body: JSON.stringify({ answers: [{}] }),
          });
          expect(post.status).toBe(200);
          expect((await post.json()).results).toHaveLength(1);
          const storage = await send('/api/store/status', { headers });
          expect(await storage.json()).toEqual({ state: 'unavailable', reason: 'disabled' });
          expect(existsSync(path.join(root, 'store'))).toBe(false);
        } finally {
          await fixture?.stop();
          await rm(root, { recursive: true, force: true });
        }
      },
      20000
    );
  }
});
