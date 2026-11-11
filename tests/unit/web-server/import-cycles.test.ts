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
import * as path from 'path';
import ts from 'typescript';

const WEB_SERVER_ROOT = path.resolve(import.meta.dir, '../../../src/web-server');

/** The one status/ cycle left in place. Paths are relative to src/web-server. */
const STATUS_CYCLE = ['status/status-view-quota.ts', 'status/status-view-types.ts'] as const;

interface ImportGraph {
  files: string[];
  edges: Map<string, string[]>;
}

function walkTypeScriptFiles(directory: string, files: string[] = []): string[] {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      walkTypeScriptFiles(fullPath, files);
      continue;
    }
    if (entry.name.endsWith('.ts') && !entry.name.endsWith('.d.ts')) {
      files.push(path.normalize(fullPath));
    }
  }
  return files;
}

function resolveRelative(fromFile: string, specifier: string): string | null {
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
      return normalized.startsWith(WEB_SERVER_ROOT) ? normalized : null;
    }
  }
  throw new Error(
    `Unresolved import "${specifier}" from ${path.relative(WEB_SERVER_ROOT, fromFile)}`
  );
}

function specifiersIn(file: string): string[] {
  const source = ts.createSourceFile(
    file,
    fs.readFileSync(file, 'utf8'),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS
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
      node.expression.kind === ts.SyntaxKind.ImportKeyword &&
      node.arguments[0] &&
      ts.isStringLiteral(node.arguments[0])
    ) {
      specs.push(node.arguments[0].text);
    } else if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === 'require' &&
      node.arguments[0] &&
      ts.isStringLiteral(node.arguments[0])
    ) {
      specs.push(node.arguments[0].text);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return specs;
}

function buildImportGraph(): ImportGraph {
  const files = walkTypeScriptFiles(WEB_SERVER_ROOT);
  const known = new Set(files);
  const edges = new Map<string, string[]>();
  for (const file of files) {
    const targets = new Set<string>();
    for (const specifier of specifiersIn(file)) {
      const resolved = resolveRelative(file, specifier);
      if (resolved && known.has(resolved) && resolved !== file) targets.add(resolved);
    }
    edges.set(file, [...targets].sort());
  }
  return { files, edges };
}

function relativeToWebServer(file: string): string {
  return path.relative(WEB_SERVER_ROOT, file).split(path.sep).join('/');
}

/** Strongly connected components with more than one file (import cycles). */
function cycleGroups(graph: ImportGraph): string[][] {
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
      if (component.length > 1) {
        groups.push(component.map(relativeToWebServer).sort());
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
});
