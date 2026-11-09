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

/** A complete status@1 document, so only the secret gate can refuse what is placed in it. */
function docBytes(summary: string, extra: Record<string, string> = {}): Buffer {
  return Buffer.from(
    JSON.stringify({
      schema: 'agent-reports/status@1',
      writer: 'vm.parity',
      seq: 9,
      writtenAt: '2026-11-08T12:00:00Z',
      t3Thread: 'fixture-thread',
      summary,
      ...extra,
    })
  );
}

/** The two readers' verdicts over the same bytes: the TypeScript gate and the native parse. */
async function verdicts(bytes: Buffer) {
  const handle = openStatusHandle({ file: PREBUILT });
  if ('unavailable' in handle) throw new Error(`module would not open: ${handle.unavailable}`);
  const ts = jsonFromBytes(bytes, 65536);
  const rust = JSON.parse(
    await handle.call(
      'parseStatus',
      JSON.stringify({ writer: 'vm.parity', contentBase64: bytes.toString('base64') })
    )
  ) as { ok: boolean; reason?: string };
  return { ts, rust };
}

/**
 * Token-shaped fixtures as a head and a tail, joined at runtime; no real credential appears
 * here or in the source. The split gives every family an `inside` position: between the
 * token's head and its body, where the pattern allows a separator (after `Bearer`, around a
 * credential field's colon, inside a private key's label run).
 */
function tokenFamilies(): Array<[string, string, string]> {
  const families: Array<[string, string, string]> = [
    ['a private key', '-----BEGIN ', 'PRIVATE KEY-----'],
    ['an API key', 'sk-ant-', 'a'.repeat(24)],
    ['an API key', 'sk-proj-', 'a'.repeat(24)],
    ['an API key', 'sk-', 'a'.repeat(24)],
  ];
  for (const kind of ['p', 'o', 'u', 's', 'r']) {
    families.push(['a GitHub token', `gh${kind}_`, 'b'.repeat(30)]);
  }
  families.push(['a GitHub token', 'github_pat_', 'b'.repeat(30)]);
  for (const kind of ['a', 'b', 'p', 'o', 's', 'r']) {
    families.push(['a Slack token', `xox${kind}-`, 'c'.repeat(12)]);
  }
  families.push(
    ['an AWS key', 'AKIA', '0DEF'.repeat(4)],
    ['a GitLab token', 'glpat-', 'e'.repeat(22)],
    ['an npm token', 'npm_', 'f'.repeat(36)],
    ['a Hugging Face token', 'hf_', 'g'.repeat(32)],
    ['a Google API key', 'AIza', 'h'.repeat(35)],
    ['a JSON web token', `eyJ${'a'.repeat(12)}`, `.${`eyJ${'b'.repeat(12)}`}.${'d'.repeat(12)}`],
    ['a bearer token', 'Bearer', ' ' + 'i'.repeat(24)],
    ['an email address', 'agent@', 'example.co'],
    ['a credential field', '"password"', ':"abcdef"'],
    ['a credential in a URL', '?token=', 'i'.repeat(24)]
  );
  return families;
}

function range(from: number, to: number): number[] {
  return Array.from({ length: to - from + 1 }, (_, at) => from + at);
}

/** JavaScript's `\s` (White_Space plus U+FEFF), each character on its own. */
const JS_WHITESPACE = [
  0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0xa0, 0x1680, ...range(0x2000, 0x200a), 0x2028, 0x2029,
  0x202f, 0x205f, 0x3000, 0xfeff,
].map((code) => String.fromCodePoint(code));

describe('the secret gate across the two readers', () => {
  withModule(
    'a non-ASCII letter beside a token in each family refuses exactly what the TypeScript reader refuses',
    async () => {
      // A non-ASCII letter is a word character to Unicode word boundaries but not to
      // JavaScript's ASCII ones; both readers must give the same verdict with it on
      // either side of every token family.
      const cjk = '\u5bc6';
      for (const [kind, head, tail] of tokenFamilies()) {
        for (const [side, summary] of [
          ['before', `${cjk}${head}${tail}`],
          ['after', `${head}${tail}${cjk}`],
        ] as const) {
          const bytes = docBytes(summary);
          const { ts, rust } = await verdicts(bytes);
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
      const { ts, rust } = await verdicts(clean);
      expect(ts.ok).toBe(rust.ok);
      expect(ts.ok).toBe(true);
    }
  );

  withModule(
    "every character of JavaScript's whitespace set, and its neighbors, decides exactly as the TypeScript reader does",
    async () => {
      // Each character JavaScript counts as `\s` (the shared White_Space set plus U+FEFF),
      // each whitespace character it does not (U+0085, U+180E), a non-ASCII letter and an
      // ASCII letter, placed before the token, inside it (between its head and body, where
      // the pattern allows a separator) and after it. The token sits in a field the status
      // projection ignores, so only the secret gate answers and the two gates are compared
      // like for like.
      const chars = [...JS_WHITESPACE, '\u0085', '\u180e', '\u5bc6', 'A'];
      const families = tokenFamilies();
      let compared = 0;
      for (const [kind, head, tail] of families) {
        for (const char of chars) {
          const code = char.codePointAt(0) as number;
          for (const [where, token] of [
            ['before', `${char}${head}${tail}`],
            ['inside', `${head}${char}${tail}`],
            ['after', `${head}${tail}${char}`],
          ] as const) {
            const bytes = docBytes('No secrets here, just a summary.', { note: token });
            const { ts, rust } = await verdicts(bytes);
            expect(
              [kind, where, code, ts.ok],
              `${kind}, U+${code.toString(16)} ${where} the token: both readers must agree`
            ).toEqual([kind, where, code, rust.ok]);
            if (!ts.ok) {
              expect(
                (ts as { reason: string }).reason,
                `${kind}, U+${code.toString(16)} ${where}`
              ).toBe(rust.reason);
            }
            compared += 1;
          }
        }
      }
      // Every family, every character, all three placements were compared.
      expect(compared).toBe(families.length * chars.length * 3);
      // A bearer token with U+FEFF as its separator is refused by both (the round-2 finding).
      const feff = docBytes('No secrets here, just a summary.', {
        note: `Bearer\uFEFF${'i'.repeat(24)}`,
      });
      const { ts, rust } = await verdicts(feff);
      expect(ts.ok).toBe(false);
      expect(rust.ok).toBe(ts.ok);
      expect((ts as { reason: string }).reason).toBe(
        'refused: it contains what looks like a bearer token'
      );
    }
  );
});
