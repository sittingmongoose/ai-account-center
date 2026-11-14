#!/usr/bin/env node
// Capture cause/state vocabulary from production TypeScript ASTs and Python ASTs.
// Run with --check to detect source drift without updating the committed table.
import { createRequire } from 'node:module';
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
const root = fileURLToPath(new URL('../../', import.meta.url));
const require = createRequire(import.meta.url);
const ts = require('typescript');
const words = new Map();
const context = /(?:cause|reason|status|state|failure|error.?code|read_failures)/i;
const wire = /^[a-z][a-z0-9]*(?:[_-][a-z0-9]+)*$/;
function add(word, source) {
  if (!wire.test(word)) return;
  if (!words.has(word)) words.set(word, new Set());
  words.get(word).add(source);
}
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    if (entry.name.startsWith('.') || ['__tests__', 'node_modules', '__pycache__'].includes(entry.name)) return [];
    const path = resolve(dir, entry.name);
    return entry.isDirectory() ? files(path) : [path];
  });
}
for (const file of files(resolve(root, 'src')).filter(f => f.endsWith('.ts'))) {
  const source = ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true);
  function visit(node) {
    let name;
    let region;
    if (ts.isTypeAliasDeclaration(node) && context.test(node.name.text)) { name = node.name.text; region = node.type; }
    if ((ts.isPropertySignature(node) || ts.isPropertyDeclaration(node)) && context.test(node.name?.getText(source) ?? '')) {
      name = node.name.getText(source); region = node.type;
    }
    if (ts.isVariableDeclaration(node) && context.test(node.name.getText(source))) { name = node.name.getText(source); region = node.initializer; }
    if (ts.isPropertyAssignment(node) && context.test(node.name.getText(source))) { name = node.name.getText(source); region = node.initializer; }
    if (ts.isBinaryExpression(node) && context.test(node.left.getText(source)) && ts.isStringLiteral(node.right)) {
      name = node.left.getText(source); region = node.right;
    }
    if (region) {
      const collect = child => {
        if (ts.isStringLiteral(child)) add(child.text, `${relative(root, file)}:${name}`);
        ts.forEachChild(child, collect);
      };
      collect(region);
    }
    ts.forEachChild(node, visit);
  }
  visit(source);
}
const python = spawnSync('python3', ['-c', String.raw`
import ast,json,pathlib,re,sys
root=pathlib.Path(sys.argv[1]); out=[]
context=re.compile(r'(cause|reason|status|state|failure|error.?code|read_failures)',re.I)
def emit(node,path,label):
    for child in ast.walk(node):
        if isinstance(child,ast.Constant) and isinstance(child.value,str): out.append([child.value,str(path.relative_to(root))+':'+label])
for path in sorted((root/'scripts').rglob('*.py')):
    if any(p in ('__pycache__','__tests__','node_modules') for p in path.parts) or path.name.startswith('test'): continue
    tree=ast.parse(path.read_text(),filename=str(path))
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
const fingerprints = Object.fromEntries([...new Set(table.flatMap(row => row.sources.map(s => s.split(':')[0])))].sort()
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
