/**
 * The secret filter's verdict corpus, recorded (73322befc) from the last TypeScript implementation
 * before the one filter moved into Rust (src/shared/secret-policy.ts `findSecret`/`findSecretIn` and
 * src/web-server/status/typed-secrets.ts `findTypedSecret`). Inputs: every document of the
 * shared refusal fixture (tests/fixtures/secret-policy-cases.json) in both source forms, and
 * every typed-secret text the Status review suite pins (status-server-review.test.ts), with the
 * long linear-time notes as repeat specs. `crates/aac-status/tests/secret_filter.rs` requires
 * byte-equal verdicts from the Rust filter. TypeScript's entry points now call that filter
 * through the native module, so `--check` proves the TS-to-Rust path end to end. There is no
 * TypeScript implementation left to record from: regenerating needs `--record` and is only
 * meaningful at 73322befc.
 *
 *   bun tests/spec/secret-filter-vectors.ts --check    # compare (needs the aac-status module)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { findSecret, findSecretIn } from '../../src/shared/secret-policy';
import { findTypedSecret } from '../../src/web-server/status/typed-secrets';

const root = fileURLToPath(new URL('../..', import.meta.url));
const out = path.join(root, 'tests/spec/secret-filter.json');
const docs = JSON.parse(
  fs.readFileSync(path.join(root, 'tests/fixtures/secret-policy-cases.json'), 'utf8')
) as Array<{ name: string; doc: unknown; secret: string | null }>;

// The typed-secret texts: every `for (const text of [ ... ])` list in the review suite's
// "typed secrets:" tests, and the linear-time test's fillers.
const review = fs.readFileSync(
  path.join(root, 'tests/unit/web-server/status/status-server-review.test.ts'),
  'utf8'
);
const texts: string[] = [];
let fillers: string[] = [];
for (const block of review.split(/\n\s*it\(/).filter((b) => b.startsWith("'typed secrets:"))) {
  const lists = [...block.matchAll(/for \(const (text|filler) of \[([\s\S]*?)\]\)/g)];
  for (const [, name, body] of lists) {
    // The linear-time test's inner list builds its notes from the filler: recorded as repeats below.
    if (name === 'text' && body.includes('filler')) continue;
    const values = new Function(`return [${body}];`)() as string[];
    if (name === 'filler') fillers = values;
    else texts.push(...values);
  }
}
assert.ok(texts.length > 50, `found ${texts.length} typed texts`);
assert.ok(fillers.length > 10, `found ${fillers.length} fillers`);

const typed = texts.map((text) => ({
  text,
  typed: findTypedSecret(text),
  secret: findSecret(text),
  secretIn: findSecretIn(text, text),
}));
// The review suite's two long notes per filler: the filler to 4000 characters, and 1000 times then `x`.
const repeats = fillers.flatMap((filler) =>
  [
    { filler, times: Math.floor(4000 / filler.length), suffix: '' },
    { filler, times: 1000, suffix: 'x' },
  ].map((spec) => {
    const text = spec.filler.repeat(spec.times) + spec.suffix;
    return { ...spec, typed: findTypedSecret(text), secret: findSecret(text) };
  })
);
const documents = docs.map(({ name, doc, secret }) => ({
  name,
  doc,
  fixture: secret,
  empty: findSecretIn('', doc),
  stringified: findSecretIn(JSON.stringify(doc), doc),
  text: findSecret(JSON.stringify(doc)),
}));

const corpus = { version: 1, typed, repeats, documents };
if (process.argv.includes('--check')) {
  const held = JSON.parse(fs.readFileSync(out, 'utf8'));
  assert.deepEqual(JSON.parse(JSON.stringify(corpus)), held);
  console.log(
    `secret-filter vectors: ${typed.length} texts, ${repeats.length} long notes, ${documents.length} documents match`
  );
} else if (process.argv.includes('--record')) {
  fs.writeFileSync(out, `${JSON.stringify(corpus, null, 2)}\n`);
  console.log(`wrote ${out} (${typed.length} texts, ${repeats.length} long notes, ${documents.length} documents)`);
} else {
  throw new Error('check only: pass --check (see the header for --record)');
}
