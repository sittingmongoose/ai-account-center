#!/usr/bin/env node
// Pin supported Rust root-key fixtures to the exact YAML reader used by today's TS loader.
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
const require = createRequire(import.meta.url);
const yaml = require('js-yaml');
const cases = JSON.parse(readFileSync(new URL('../../crates/aac-config/tests/fixtures/yaml-key-forms.json', import.meta.url), 'utf8'));
const generated = yaml.dump({ dashboard: { port: 3104 }, cliproxy: { enabled: true } }, {
  indent: 2, lineWidth: -1, quotingType: '"',
});
if (cases.find(test => test.name === 'writer')?.source !== generated) throw new Error('TS YAML writer fixture drift');
let checked = 0;
for (const test of cases) {
  for (const prefix of ['', '\ufeff']) {
    let accepted = false;
    try {
      const value = yaml.load(prefix + test.source);
      accepted = value?.dashboard?.port === 3104 && Object.hasOwn(value, 'cliproxy');
    } catch { /* TS also refuses malformed and ambiguous documents. */ }
    if (accepted !== test.tsAccept || (test.accept && !accepted)) throw new Error(`TS YAML reader drift: ${test.name}`);
    checked++;
  }
}
console.log(`${checked} YAML inputs checked against the TS reader; unsupported layouts retain explicit refusal expectations`);
