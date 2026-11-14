#!/usr/bin/env node
// Execute the current TS functions in an isolated VM context. No server or private home loads.
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import vm from 'node:vm';
const root = fileURLToPath(new URL('../../', import.meta.url));
const require = createRequire(import.meta.url);
const ts = require('typescript');
function functions(path, names) {
  const text = readFileSync(resolve(root, path), 'utf8');
  const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
  const picked = source.statements.filter(node => {
    if (ts.isFunctionDeclaration(node)) return names.includes(node.name?.text);
    return ts.isVariableStatement(node) && node.declarationList.declarations.some(d => names.includes(d.name.getText(source)));
  });
  if (picked.length !== names.length) throw new Error(`identity function drift in ${path}`);
  const javascript = ts.transpileModule(picked.map(n => n.getText(source)).join('\n'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const context = { createHash, exports: {} };
  vm.runInNewContext(javascript, context, { filename: path, timeout: 1000 });
  return context.exports;
}
const sessions = functions('src/web-server/usage/analytics-session-key.ts', [
  'SESSION_KEY_NAMESPACE', 'SESSION_KEY_LENGTH', 'SESSION_KEY_SHAPE', 'analyticsSessionKey', 'publishedSessionKey',
]);
const hashes = functions('src/web-server/usage/activity-cells.ts', ['analyticsSessionHash']);
const accounts = functions('src/web-server/services/account-analytics-history.ts', ['accountAnalyticsIdentity']);
const eventSource = 'native/aac-store/src/log.rs';
if (!readFileSync(resolve(root, eventSource), 'utf8').includes('format!("{}-{:016x}", self.short, self.next_seq)')) {
  throw new Error('store event-id algorithm changed');
}
const output = {
  schemaVersion: 1,
  sources: {
    sessions: ['src/web-server/usage/analytics-session-key.ts', 'src/web-server/usage/activity-cells.ts'],
    accounts: ['src/web-server/services/account-analytics-history.ts'],
    events: [eventSource],
  },
  sessions: [
    ['codex', 'fixture-session'], ['claude', ''], ['muse', '0123456789abcdef'],
    ['codex', '0123456789ABCDEF'], ['omp', '\u03bb-session'], ['zcode', 'line\nseparator'],
    ['claude-code', 'path/with:punctuation'], ['codex', '\u{1f600}'],
  ].map(([tool, sessionId]) => ({ tool, sessionId,
    sessionKey: sessions.analyticsSessionKey(tool, sessionId), sessionHash: hashes.analyticsSessionHash(tool, sessionId),
    publishedSessionKey: sessions.publishedSessionKey(tool, sessionId),
  })),
  accounts: [
    {provider:'codex', id:'fixture-account'}, {provider:'claude', id:'fixture-account', email:null},
    {provider:'muse', id:'fixture-account', email:'ALIAS'}, {provider:'muse', id:'fixture-account', email:'alias'},
    {provider:'codex', id:'\u03bb-identity', email:''}, {provider:'codex', id:'quote"slash\\', email:'\u039f\u03a3'},
  ].map(input => ({...input, accountKey:accounts.accountAnalyticsIdentity(input)})),
  // Store event ids are assigned in native Rust today, not hashed by TypeScript. BigInt
  // reproduces the pinned existing format for both ends of the u64 sequence range.
  events: [['fixture', '0'], ['fixture', '1'], ['fixture', '9007199254740993'], ['fixture', '18446744073709551615']]
    .map(([storeShortId, sequence]) => ({storeShortId, sequence, eventId:`${storeShortId}-${BigInt(sequence).toString(16).padStart(16,'0')}`})),
};
const path = resolve(root, 'tests/spec/id-vectors.json');
const bytes = JSON.stringify(output, null, 2) + '\n';
if (process.argv.includes('--check')) {
  if (readFileSync(path, 'utf8') !== bytes) throw new Error('identity vectors changed; regenerate and review');
} else writeFileSync(path, bytes);
console.log(`${output.sessions.length} session, ${output.accounts.length} account, ${output.events.length} event vectors`);
