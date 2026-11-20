import { describe, expect, it } from 'bun:test';
import { existsSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
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

  it('publishes live account samples without the history store', async () => {
    const root = await mkdtemp(path.join(os.tmpdir(), 'aac-status-live-usage-'));
    let fixture: Awaited<ReturnType<typeof startNativeStoreServer>> | undefined;
    try {
      // An unbound profile gives the collector a real account row without a provider probe.
      const account = {
        id: 'antigravity:acct:00000001',
        label: 'Fixture account',
        platform: 'mac',
        provider: 'antigravity',
        credential: { kind: 'antigravity-profile', profileId: 'fixture' },
      };
      await writeFile(
        path.join(root, 'account-usage-accounts.json'),
        JSON.stringify({ version: 2, accounts: [account] }),
        { mode: 0o600 }
      );
      fixture = await startNativeStoreServer(root, 'off', { collection: true, worker });
      const { send, cookie, url } = fixture;
      const headers = { cookie, origin: url };
      const dashboard = await send('/api/accounts/dashboard', { headers });
      expect(dashboard.status).toBe(200);
      expect((await dashboard.json()).accounts).toContainEqual(
        expect.objectContaining({ id: account.id })
      );
      const deadline = Date.now() + 2000;
      let state;
      do {
        const status = await send('/api/status', { headers });
        expect(status.status).toBe(200);
        state = (await status.json()).sources.find(
          (source: { id: string }) => source.id === 'usage'
        )?.state;
        if (state === 'ok') break;
        await Bun.sleep(50);
      } while (Date.now() < deadline);
      expect(state).toBe('ok');
      expect(existsSync(path.join(root, 'store'))).toBe(false);
    } finally {
      await fixture?.stop();
      await rm(root, { recursive: true, force: true });
    }
  }, 15000);
});
