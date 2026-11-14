/** Capture literal probe requests from the unchanged TS tests with their real parser. */
import fs from 'node:fs';
import ts from 'typescript';
import { parseMachineProbe } from '../../../src/web-server/status/status-collectors';
const file = 'tests/unit/web-server/status/status-collectors.test.ts';
const source = ts.createSourceFile(
  file,
  fs.readFileSync(file, 'utf8'),
  ts.ScriptTarget.Latest,
  true
);
const cases: unknown[] = [];
function visit(node: ts.Node) {
  if (
    ts.isCallExpression(node) &&
    node.expression.getText(source) === 'parseMachineProbe' &&
    node.arguments.length === 2 &&
    node.arguments.every(ts.isStringLiteralLike)
  ) {
    const [platform, probe] = node.arguments as unknown as [
      ts.StringLiteralLike,
      ts.StringLiteralLike,
    ];
    const args = { platform: platform.text, probe: probe.text };
    cases.push({
      source: `${file}:${source.getLineAndCharacterOfPosition(node.pos).line + 1}`,
      verb: 'inventory',
      args,
      expected: parseMachineProbe(args.platform as 'linux', args.probe),
    });
  }
  ts.forEachChild(node, visit);
}
visit(source);
fs.writeFileSync(
  'tests/spec/agent/inventory.json',
  JSON.stringify({ schemaVersion: 1, cases }, null, 2) + '\n'
);
console.log(`${cases.length} inventory cases`);
