#!/usr/bin/env node
// Execute the actual TS resolver with injected cwd/default home; never load private files.
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
const require = createRequire(import.meta.url);
const ts = require('typescript');
const source = ts.createSourceFile(
  'config-manager.ts',
  readFileSync(new URL('../../src/utils/config-manager.ts', import.meta.url), 'utf8'),
  ts.ScriptTarget.Latest,
  true
);
const names = ['setGlobalConfigDir', '_resolveCcsDir', 'getCcsDir'];
const functions = source.statements.filter(
  (node) => ts.isFunctionDeclaration(node) && names.includes(node.name?.text)
);
if (functions.length !== names.length) throw new Error('TS root resolver moved');
const code = ts.transpileModule(functions.map((node) => node.getText(source)).join('\n'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText;
const rows = JSON.parse(
  readFileSync(
    new URL('../../crates/aac-config/tests/fixtures/root-paths.json', import.meta.url),
    'utf8'
  )
);
const root = '/fixture';
const expand = (value) => value.replaceAll('$ROOT', root);
for (const row of rows) {
  const context = {
    exports: {},
    _globalConfigDir: undefined,
    configScopeStorage: { getStore: () => undefined },
    process: {
      env: Object.fromEntries(
        Object.entries(row.environment).map(([key, value]) => [key, expand(value)])
      ),
    },
    os: { homedir: () => path.join(root, 'default-home') },
    path: { ...path, resolve: (...parts) => path.resolve(root, 'cwd', ...parts) },
  };
  vm.runInNewContext(code, context, { timeout: 1000 });
  if (row.cli !== null) context.exports.setGlobalConfigDir(expand(row.cli));
  if (context.exports.getCcsDir() !== path.join(root, row.expected))
    throw new Error(`TS root mismatch: ${row.name}`);
}
console.log(`${rows.length} root-path cases match the current TS resolver`);
