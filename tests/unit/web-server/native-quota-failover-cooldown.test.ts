import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import {
  getCodexProfileQuotaRows,
  resetNativeQuotaState,
} from '../../../src/web-server/usage/native-quota-collector';
import { codexUsageIdentity } from '../../../src/web-server/services/usage-failover-codex-identity';
import {
  holdIdentityUsageCooldown,
  readIdentityUsageCooldown,
} from '../../../src/web-server/services/usage-failover-cooldown';
import type { CodexQuotaResult } from '../../../src/cliproxy/quota/quota-types';

function auth(workspace = 'fixture-workspace', token = 'fixture-private-token') {
  const payload = {
    email: 'fixture@example.test',
    sub: 'fixture-person',
    iss: 'https://auth.openai.com',
    'https://api.openai.com/auth': {
      chatgpt_account_id: workspace,
      chatgpt_user_id: 'fixture-person',
    },
  };
  return {
    tokens: {
      access_token: token,
      account_id: workspace,
      id_token: `e30.${Buffer.from(JSON.stringify(payload)).toString('base64url')}.fixture`,
    },
  };
}

function writeAuth(home: string, name: string, value = auth()): void {
  const file = path.join(home, '.ccs', 'codex-instances', name, 'auth.json');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

const limited = (retryAfter?: number): CodexQuotaResult => ({
  success: false,
  windows: [],
  planType: null,
  lastUpdated: 0,
  accountId: 'fixture-workspace',
  httpStatus: 429,
  retryable: true,
  error: 'rate limited',
  ...(retryAfter === undefined ? {} : { errorDetail: `retry-after:${retryAfter}` }),
});

describe('native Codex identity-wide provider cooldown', () => {
  it('checks a peer hold at the actual network boundary, including force refresh and a fresh alias', async () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-native-identity-hold-'));
    try {
      await runWithScopedCcsHome(home, async () => {
        const now = Date.now();
        const ccs = path.join(home, '.ccs');
        const identity = codexUsageIdentity(auth().tokens)!;
        writeAuth(home, 'first');
        writeAuth(home, 'alias');
        let fetches = 0;
        const deps = {
          defaultCodexProfile: () => null,
          now: () => now,
          fetchCodexQuotaWithToken: async () => {
            fetches++;
            return limited();
          },
        };
        // The collector yields after capturing auth and before making its request.
        const pending = getCodexProfileQuotaRows(['first'], deps, { force: true });
        holdIdentityUsageCooldown(ccs, 'codex', identity, now + 120_000, now);
        await pending;
        await getCodexProfileQuotaRows(['alias'], deps, { force: true });
        expect(fetches).toBe(0);
      });
    } finally {
      resetNativeQuotaState();
      fs.rmSync(home, { recursive: true, force: true });
    }
  });

  for (const retryAfter of [undefined, 600]) {
    it(`shares a native 429 across aliases and token rotation after restart (Retry-After ${retryAfter ?? 'absent'})`, async () => {
      const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-native-429-hold-'));
      try {
        await runWithScopedCcsHome(home, async () => {
          let now = Date.now();
          const ccs = path.join(home, '.ccs');
          const identity = codexUsageIdentity(auth().tokens)!;
          writeAuth(home, 'first');
          writeAuth(home, 'alias');
          writeAuth(home, 'other', auth('other-workspace'));
          let fetches = 0;
          const deps = {
            defaultCodexProfile: () => null,
            now: () => now,
            fetchCodexQuotaWithToken: async () => {
              fetches++;
              return limited(retryAfter);
            },
          };
          await getCodexProfileQuotaRows(['first'], deps, { force: true });
          const until = now + (retryAfter ?? 60) * 1000;
          expect(readIdentityUsageCooldown(ccs, 'codex', identity, now)).toBe(until);
          resetNativeQuotaState();
          writeAuth(home, 'alias', auth('fixture-workspace', 'fixture-rotated-token'));
          now += 59_000;
          await getCodexProfileQuotaRows(['alias'], deps, { force: true });
          expect(fetches).toBe(1);
          await getCodexProfileQuotaRows(['other'], deps, { force: true });
          expect(fetches).toBe(2);
          now = until + 1;
          await getCodexProfileQuotaRows(['alias'], deps, { force: true });
          expect(fetches).toBe(3);
        });
      } finally {
        resetNativeQuotaState();
        fs.rmSync(home, { recursive: true, force: true });
      }
    });
  }
});
