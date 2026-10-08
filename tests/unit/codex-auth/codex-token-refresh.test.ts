import { describe, expect, it } from 'bun:test';
import {
  CODEX_REFRESH_URL,
  refreshCodexTokens,
  type CodexTokenFetch,
} from '../../../src/codex-auth/codex-token-refresh';

function respond(status: number, body: unknown): { fetch: CodexTokenFetch; urls: string[] } {
  const urls: string[] = [];
  return {
    urls,
    fetch: async (url, init) => {
      urls.push(url);
      expect(init.redirect).toBe('manual');
      expect(init.signal).toBeInstanceOf(AbortSignal);
      return {
        status,
        text: async () => (typeof body === 'string' ? body : JSON.stringify(body)),
      };
    },
  };
}

describe('refreshCodexTokens', () => {
  it('posts to the fixed endpoint even when Codex override variables are set', async () => {
    const previous = process.env.CODEX_REFRESH_TOKEN_URL_OVERRIDE;
    process.env.CODEX_REFRESH_TOKEN_URL_OVERRIDE = 'https://attacker.example.test/token';
    try {
      const http = respond(200, { access_token: 'at-FAKE' });
      const result = await refreshCodexTokens('rt-FAKE', { fetch: http.fetch });
      expect(http.urls).toEqual([CODEX_REFRESH_URL]);
      expect(result).toEqual({ kind: 'ok', tokens: { accessToken: 'at-FAKE' } });
    } finally {
      if (previous === undefined) delete process.env.CODEX_REFRESH_TOKEN_URL_OVERRIDE;
      else process.env.CODEX_REFRESH_TOKEN_URL_OVERRIDE = previous;
    }
  });

  it.each([
    [401, { error: { code: 'refresh_token_reused' } }, 'refresh_token_reused'],
    [401, { error: 'refresh_token_expired' }, 'refresh_token_expired'],
    [401, { code: 'refresh_token_invalidated' }, 'refresh_token_invalidated'],
    [401, 'not json rt-FAKE', 'unauthorized'],
    [400, { error: 'invalid_grant', error_description: 'rt-FAKE' }, 'invalid_grant'],
  ])('classifies %i %j as dead (%s)', async (status, body, code) => {
    const result = await refreshCodexTokens('rt-FAKE', { fetch: respond(status, body).fetch });
    expect(result).toEqual({ kind: 'dead', code: code as never, httpStatus: status });
  });

  it.each([
    [400, { error: 'invalid_request' }, 'http_error'],
    [403, '<html>challenge</html>', 'http_error'],
    [307, '', 'http_error'],
    [429, {}, 'rate_limited'],
    [500, {}, 'server_error'],
    [503, 'busy', 'server_error'],
  ])('classifies %i as transient (%s)', async (status, body, code) => {
    const result = await refreshCodexTokens('rt-FAKE', { fetch: respond(status, body).fetch });
    expect(result).toEqual({ kind: 'transient', code: code as never, httpStatus: status });
  });

  it('reports timeouts and network failures as transient without their text', async () => {
    const timeout = await refreshCodexTokens('rt-FAKE', {
      fetch: async () => {
        throw Object.assign(new Error('rt-FAKE timed out'), { name: 'TimeoutError' });
      },
    });
    expect(timeout).toEqual({ kind: 'transient', code: 'timeout', httpStatus: null });
    const network = await refreshCodexTokens('rt-FAKE', {
      fetch: async () => {
        throw new TypeError('fetch failed rt-FAKE');
      },
    });
    expect(network).toEqual({ kind: 'transient', code: 'network', httpStatus: null });
  });

  it('aborts a hung request at the timeout', async () => {
    const result = await refreshCodexTokens('rt-FAKE', {
      timeoutMs: 20,
      fetch: (_url, init) =>
        new Promise((_resolve, reject) =>
          init.signal.addEventListener('abort', () => reject(init.signal.reason))
        ),
    });
    expect(result).toEqual({ kind: 'transient', code: 'timeout', httpStatus: null });
  });

  it.each([
    ['{"access_token": '],
    [{ id_token: 'x' }],
    [{ access_token: '' }],
    [{ access_token: 'at', refresh_token: 42 }],
    [[1, 2]],
  ])('rejects a malformed success body %j', async (body) => {
    const result = await refreshCodexTokens('rt-FAKE', { fetch: respond(200, body).fetch });
    expect(result).toEqual({ kind: 'invalid_response', httpStatus: 200 });
  });

  it('returns every token of a full response', async () => {
    const result = await refreshCodexTokens('rt-FAKE', {
      fetch: respond(200, { id_token: 'id', access_token: 'at', refresh_token: 'rt2' }).fetch,
    });
    expect(result).toEqual({
      kind: 'ok',
      tokens: { idToken: 'id', accessToken: 'at', refreshToken: 'rt2' },
    });
  });
});
