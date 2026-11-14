import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import assert from 'node:assert/strict';
const file = new URL('parser-vectors.json', import.meta.url);
const source = 'src/web-server/services/claude-desktop-live-service.ts';
const text = fs.readFileSync(new URL('../../' + source, import.meta.url), 'utf8');
const ast = ts.createSourceFile(source, text, ts.ScriptTarget.Latest, true);
const fn = ast.statements.find(
  (n) => ts.isFunctionDeclaration(n) && n.name?.text === 'nonnegative'
);
assert.ok(fn, 'TS counterpart changed');
const code = ts.transpileModule(fn.getText(ast), {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
}).outputText;
const context = vm.createContext({});
vm.runInContext(code, context);
const data = JSON.parse(fs.readFileSync(file));
let compared = 0;
for (const row of data.cases) {
  // Only these two providers have the same nonnegative scalar boundary in TS.
  if (row.kind !== 'number' || !['claude_usage', 'plan_common'].includes(row.module)) continue;
  const result = context.nonnegative(row.input);
  if (process.argv.includes('--regenerate')) {
    row.typescriptSource = source + '#nonnegative';
    row.typescript = result;
    row.disagreement = JSON.stringify(result) !== JSON.stringify(row.python);
  } else {
    assert.equal(result, row.typescript, JSON.stringify(row));
    assert.equal(row.disagreement, JSON.stringify(result) !== JSON.stringify(row.python));
  }
  compared++;
}
if (process.argv.includes('--regenerate'))
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n');
console.log(
  `${compared} TS parser cases checked; ${data.cases.filter((r) => r.disagreement).length} disagreements retained`
);
