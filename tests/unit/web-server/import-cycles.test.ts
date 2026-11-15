/**
 * Import cycles under src/web-server.
 *
 * The leave-alone status/ cycle (untangle audit U3) is named here and is the
 * only one this check allows:
 * status/status-view-quota.ts <-> status/status-view-types.ts.
 * Any other cycle, including a new one inside status/, fails the suite.
 */
import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import ts from 'typescript';

const WEB_SERVER_ROOT = path.resolve(import.meta.dir, '../../../src/web-server');

/** The one status/ cycle left in place. Paths are relative to src/web-server. */
const STATUS_CYCLE = ['status/status-view-quota.ts', 'status/status-view-types.ts'] as const;

interface ImportGraph {
  files: string[];
  edges: Map<string, string[]>;
}

/** Every source the resolver can name: .ts, .tsx and .mts, never declaration files. */
function walkTypeScriptFiles(directory: string, files: string[] = []): string[] {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      walkTypeScriptFiles(fullPath, files);
      continue;
    }
    if (/\.(ts|tsx|mts)$/.test(entry.name) && !/\.d\.m?ts$/.test(entry.name)) {
      files.push(path.normalize(fullPath));
    }
  }
  return files;
}

function resolveRelative(fromFile: string, specifier: string, root: string): string | null {
  if (!specifier.startsWith('.')) return null;
  const base = path.resolve(path.dirname(fromFile), specifier);
  const candidates = [
    base,
    `${base}.ts`,
    `${base}.tsx`,
    `${base}.mts`,
    path.join(base, 'index.ts'),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
      const normalized = path.normalize(candidate);
      return normalized.startsWith(root) ? normalized : null;
    }
  }
  throw new Error(`Unresolved import "${specifier}" from ${path.relative(root, fromFile)}`);
}

/** A module name written as a plain string or a template with no substitutions. */
function literalText(node: ts.Node | undefined): string | null {
  return node && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node))
    ? node.text
    : null;
}

function specifiersIn(file: string): string[] {
  const source = ts.createSourceFile(
    file,
    fs.readFileSync(file, 'utf8'),
    ts.ScriptTarget.Latest,
    true,
    file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS
  );
  const specs: string[] = [];
  const visit = (node: ts.Node): void => {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      specs.push(node.moduleSpecifier.text);
    } else if (
      ts.isImportEqualsDeclaration(node) &&
      ts.isExternalModuleReference(node.moduleReference) &&
      ts.isStringLiteral(node.moduleReference.expression)
    ) {
      specs.push(node.moduleReference.expression.text);
    } else if (
      ts.isCallExpression(node) &&
      (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
        (ts.isIdentifier(node.expression) && node.expression.text === 'require')) &&
      literalText(node.arguments[0]) !== null
    ) {
      // import('./x'), import(`./x`), require('./x') and require(`./x`).
      specs.push(literalText(node.arguments[0])!);
    } else if (
      ts.isImportTypeNode(node) &&
      ts.isLiteralTypeNode(node.argument) &&
      literalText(node.argument.literal) !== null
    ) {
      // A type written as import('./x').Name is an edge, as `import type` is.
      specs.push(literalText(node.argument.literal)!);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return specs;
}

function buildImportGraph(root = WEB_SERVER_ROOT): ImportGraph {
  const files = walkTypeScriptFiles(root);
  const known = new Set(files);
  const edges = new Map<string, string[]>();
  for (const file of files) {
    const targets = new Set<string>();
    for (const specifier of specifiersIn(file)) {
      const resolved = resolveRelative(file, specifier, root);
      if (resolved && known.has(resolved)) targets.add(resolved);
    }
    edges.set(file, [...targets].sort());
  }
  return { files, edges };
}

/** Import cycles: strongly connected components with more than one file, or a file importing itself. */
function cycleGroups(graph: ImportGraph, root = WEB_SERVER_ROOT): string[][] {
  const relative = (file: string) => path.relative(root, file).split(path.sep).join('/');
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const stack: string[] = [];
  const onStack = new Set<string>();
  let next = 0;
  const groups: string[][] = [];

  const strong = (node: string): void => {
    index.set(node, next);
    low.set(node, next);
    next += 1;
    stack.push(node);
    onStack.add(node);
    for (const target of graph.edges.get(node) ?? []) {
      if (!index.has(target)) {
        strong(target);
        low.set(node, Math.min(low.get(node) ?? 0, low.get(target) ?? 0));
      } else if (onStack.has(target)) {
        low.set(node, Math.min(low.get(node) ?? 0, index.get(target) ?? 0));
      }
    }
    if (low.get(node) === index.get(node)) {
      const component: string[] = [];
      let cursor: string | undefined;
      do {
        cursor = stack.pop();
        if (!cursor) break;
        onStack.delete(cursor);
        component.push(cursor);
      } while (cursor !== node);
      if (component.length > 1 || (graph.edges.get(node) ?? []).includes(node)) {
        groups.push(component.map(relative).sort());
      }
    }
  };

  for (const file of graph.files) {
    if (!index.has(file)) strong(file);
  }
  return groups.sort((left, right) => left.join('\0').localeCompare(right.join('\0')));
}

function sameGroup(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((file, index) => file === right[index]);
}

describe('src/web-server import cycles', () => {
  const groups = cycleGroups(buildImportGraph());

  it('allows only the named status/ cycle', () => {
    const allowed = [...STATUS_CYCLE].sort();
    const unexpected = groups.filter((group) => !sameGroup(group, allowed));
    expect(unexpected).toEqual([]);
  });

  it('still sees the named status/ cycle, so the exception is not a no-op', () => {
    const allowed = [...STATUS_CYCLE].sort();
    expect(groups.some((group) => sameGroup(group, allowed))).toBe(true);
  });

  it('sees template and type-position imports, self-imports and .tsx/.mts files (U3 recheck L1)', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-import-cycles-'));
    try {
      const write = (name: string, text: string) => fs.writeFileSync(path.join(root, name), text);
      write('a.ts', 'export const a = () => import(`./b`);\n');
      write('b.ts', "export type B = import('./a').A;\n");
      write('self.ts', 'export const self = require(`./self`);\n');
      write('view.tsx', "import { m } from './model';\nexport const v = m;\n");
      write('model.mts', "import type { v } from './view';\nexport const m = 1;\n");
      write('plain.ts', "import { a } from './a';\nexport const p = a;\n");
      write('types.d.ts', "import './plain';\n");
      expect(cycleGroups(buildImportGraph(root), root)).toEqual([
        ['a.ts', 'b.ts'],
        ['model.mts', 'view.tsx'],
        ['self.ts'],
      ]);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
