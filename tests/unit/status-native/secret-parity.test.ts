import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import { openStatusHandle } from '../../../src/status-native/pinned';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';
import { jsonFromBytes } from '../../../src/web-server/status/schema-helpers';

/** The module `bun run status:build` leaves behind; the parity check needs the real reader. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;

/** A complete status@1 document with the given summary, so only the secret gate can refuse. */
function docBytes(summary: string): Buffer {
  return Buffer.from(
    JSON.stringify({
      schema: 'agent-reports/status@1',
      writer: 'vm.parity',
      seq: 9,
      writtenAt: '2026-11-08T12:00:00Z',
      t3Thread: 'fixture-thread',
      summary,
    })
  );
}

/** Token-shaped fixtures, joined at runtime; no real credential appears here or in the source. */
function tokenFamilies(): Array<[string, string]> {
  return [
    ['an API key', ['sk', 'ant', 'a'.repeat(24)].join('-')],
    ['an API key', ['sk', 'a'.repeat(24)].join('-')],
    ['a GitHub token', ['gh', 'p_'].join('') + 'b'.repeat(30)],
    ['a GitHub token', 'github_pat_' + 'b'.repeat(30)],
    ['a Slack token', 'xoxa-' + 'c'.repeat(12)],
    ['an AWS key', 'AKIA' + '0DEF'.repeat(4)],
    ['a GitLab token', 'glpat-' + 'e'.repeat(22)],
    ['an npm token', 'npm_' + 'f'.repeat(36)],
    ['a Hugging Face token', 'hf_' + 'g'.repeat(32)],
    ['a Google API key', 'AIza' + 'h'.repeat(35)],
    [
      'a JSON web token',
      ['eyJhbGciOiJIUzI1NiJ5dCI6IkpXVCJ9', 'eyJzdWIiOiIxMjM0NTY3ODkwIn0', 'signaturepart'].join(
        '.'
      ),
    ],
    ['a bearer token', ['Bear', 'er '].join('') + 'i'.repeat(24)],
    ['an email address', 'agent@example.co'],
  ];
}

describe('the secret gate across the two readers', () => {
  withModule(
    'a non-ASCII letter beside a token in each family refuses exactly what the TypeScript reader refuses',
    async () => {
      const handle = openStatusHandle({ file: PREBUILT });
      if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
      // A non-ASCII letter is a word character to Unicode word boundaries but not to
      // JavaScript's ASCII ones; both readers must give the same verdict with it on
      // either side of every token family.
      const cjk = '\u5bc6';
      for (const [kind, token] of tokenFamilies()) {
        for (const [side, summary] of [
          ['before', `${cjk}${token}`],
          ['after', `${token}${cjk}`],
        ] as const) {
          const bytes = docBytes(summary);
          const ts = jsonFromBytes(bytes, 65536);
          const rust = JSON.parse(
            await handle.call(
              'parseStatus',
              JSON.stringify({ writer: 'vm.parity', contentBase64: bytes.toString('base64') })
            )
          ) as { ok: boolean; reason?: string };
          expect(ts.ok, `${kind}, non-ASCII ${side} the token: both readers must agree`).toBe(
            rust.ok
          );
          if (!ts.ok) {
            expect((ts as { reason: string }).reason, `${kind}, non-ASCII ${side}`).toBe(
              rust.reason
            );
          }
        }
      }
      // And a clean document passes both.
      const clean = docBytes('No secrets here, just a summary.');
      expect(jsonFromBytes(clean, 65536).ok).toBe(
        (JSON.parse(
          await handle.call(
            'parseStatus',
            JSON.stringify({ writer: 'vm.parity', contentBase64: clean.toString('base64') })
          )
        ) as { ok: boolean }).ok
      );
    }
  );
});
