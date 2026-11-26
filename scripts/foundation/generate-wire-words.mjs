#!/usr/bin/env node
// Capture cause/state vocabulary from the native reset route and Python ASTs; rows of retired
// sources are kept from the committed table.
// Run with --check to detect source drift without updating the committed table.
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { toPosix } from './to-posix.mjs';
const root = fileURLToPath(new URL('../../', import.meta.url));
const words = new Map();
const context = /(?:cause|reason|status|state|failure|error.?code|read_failures)/i;
const wire = /^[a-z][a-z0-9]*(?:[_-][a-z0-9]+)*$/;
function add(word, source) {
  if (!wire.test(word)) return;
  if (!words.has(word)) words.set(word, new Set());
  words.get(word).add(source);
}
// The TypeScript server (src/) was retired after this table was generated. Rows whose source
// file no longer exists are carried forward from the committed table as frozen provenance, so
// regenerating never drops a word a native reader still accepts.
const committed = JSON.parse(readFileSync(resolve(root, 'tests/spec/wire-words.json'), 'utf8'));
for (const row of committed)
  for (const source of row.sources)
    if (!existsSync(resolve(root, source.split(':')[0]))) add(row.word, source);
// Reset replies now come from the native HTTP route's refusal allowlist.
const resetRoute = 'crates/aac-http/src/public_auth/reset_routes.rs';
const resetSource = readFileSync(resolve(root, resetRoute), 'utf8');
const resetReasons = /const SAFE_REASONS: &\[&str\] = &\[([^\]]+)\];/.exec(resetSource);
if (!resetReasons) throw new Error('Rust reset refusal table is missing');
for (const [, word] of resetReasons[1].matchAll(/"([^"]+)"/g)) add(word, `${resetRoute}:SAFE_REASONS`);
const resetInventoryUnavailable = /"(reset-inventory-unavailable)"/.exec(resetSource);
if (!resetInventoryUnavailable) throw new Error('Rust reset inventory refusal is missing');
add(resetInventoryUnavailable[1], `${resetRoute}:inventory`);
const python = spawnSync('python3', ['-c', String.raw`
import ast,json,pathlib,re,sys
root=pathlib.Path(sys.argv[1]); out=[]
context=re.compile(r'(cause|reason|status|state|failure|error.?code|read_failures)',re.I)
def emit(node,path,label):
    for child in ast.walk(node):
        if isinstance(child,ast.Constant) and isinstance(child.value,str): out.append([child.value,path.relative_to(root).as_posix()+':'+label])
for path in sorted((root/'scripts').rglob('*.py')):
    if any(p in ('__pycache__','__tests__','node_modules') for p in path.parts) or path.name.startswith('test'): continue
    tree=ast.parse(path.read_text(encoding='utf-8'),filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node,(ast.Assign,ast.AnnAssign)):
            targets=node.targets if isinstance(node,ast.Assign) else [node.target]
            label=' '.join(ast.unparse(t) for t in targets)
            if context.search(label) and node.value: emit(node.value,path,label)
        elif isinstance(node,ast.Dict):
            for key,value in zip(node.keys,node.values):
                if isinstance(key,ast.Constant) and isinstance(key.value,str) and context.search(key.value): emit(value,path,key.value)
        elif isinstance(node,ast.Compare):
            label=ast.unparse(node.left)
            if context.search(label):
                for value in node.comparators: emit(value,path,label)
        elif isinstance(node,ast.Call):
            label=ast.unparse(node.func)
            if re.search(r'(failure|error)$',label,re.I) and node.args: emit(node.args[0],path,label)
            for keyword in node.keywords:
                if keyword.arg and context.search(keyword.arg): emit(keyword.value,path,keyword.arg)
print(json.dumps(out))
`, root], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
if (python.status !== 0) throw new Error(python.stderr);
for (const [word, source] of JSON.parse(python.stdout)) add(word, source);
const overrides = JSON.parse(readFileSync(resolve(root, 'tests/spec/wire-causes.json'), 'utf8'));
function cause(word) {
  if (Object.hasOwn(overrides, word)) return overrides[word];
  if (/(?:timeout|timed_out|deadline)/.test(word)) return 'timeout';
  if (/(?:cancel|abort)/.test(word)) return 'cancelled';
  if (/(?:permission|access_denied)/.test(word)) return 'permission_denied';
  if (/(?:rate_limit|cooldown)/.test(word)) return 'rate_limited';
  if (/(?:identity.*mismatch|mismatch.*identity)/.test(word)) return 'identity_mismatch';
  if (/(?:team.*mismatch)/.test(word)) return 'team_mismatch';
  if (/(?:identity.*(?:missing|unavailable|unknown))/.test(word)) return 'identity_unavailable';
  if (/(?:signin|sign_in|login)/.test(word)) return 'signin_needed';
  if (/(?:auth|unauthorized)/.test(word)) return 'auth_required';
  if (/(?:ssh|unreachable|transport|disconnected)/.test(word)) return 'ssh_unreachable';
  if (/(?:not_installed|not_found|missing|absent)/.test(word)) return 'not_installed';
  if (/(?:invalid|corrupt|malformed|damaged|parse)/.test(word)) return 'damaged';
  if (/(?:blocked|policy|disabled|refused|unsupported)/.test(word)) return 'refused';
  if (/(?:held|paused)/.test(word)) return 'held';
  if (/(?:busy|locked|in_use)/.test(word)) return 'busy';
  if (/(?:failed|error|unavailable|unknown)/.test(word)) return 'provider_error';
  // Feature success/progress/absence states do not become fabricated failures.
  return null;
}
const table = [...words].sort(([a], [b]) => a.localeCompare(b, 'en')).map(([word, sources]) => ({
  word, cause: cause(word), sources: [...sources].sort(),
}));
const path = resolve(root, 'tests/spec/wire-words.json');
const output = JSON.stringify(table, null, 2) + '\n';
// Only live sources are fingerprinted; retired ones have no file left to drift.
const fingerprints = Object.fromEntries([...new Set(table.flatMap(row => row.sources.map(s => s.split(':')[0])))].filter(path => existsSync(resolve(root, path))).sort()
  .map(path => [path, createHash('sha256').update(readFileSync(resolve(root, path))).digest('hex')]));
const fingerprintPath = resolve(root, 'tests/spec/wire-source-hashes.json');
const fingerprintOutput = JSON.stringify(fingerprints, null, 2) + '\n';
if (process.argv.includes('--check')) {
  if (readFileSync(path, 'utf8') !== output) throw new Error('wire vocabulary drift; regenerate and review');
  if (readFileSync(fingerprintPath, 'utf8') !== fingerprintOutput) throw new Error('wire vocabulary source drift; regenerate and review');
} else {
  writeFileSync(path, output);
  writeFileSync(fingerprintPath, fingerprintOutput);
}
console.log(`${table.length} wire words from ${new Set(table.flatMap(row => row.sources.map(s => s.split(':')[0]))).size} sources`);
