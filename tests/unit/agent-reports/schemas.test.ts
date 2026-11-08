import { describe, expect, it } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const rootDir = join(import.meta.dir, '../../../docs/agent-reports');
const schemasDir = join(rootDir, 'schemas');
const examplesDir = join(rootDir, 'examples');

const KINDS = ['thread', 'tasks', 'card', 'answer', 'run', 'status'] as const;

const ALLOWED_KEYWORDS = new Set([
  '$schema',
  '$id',
  '$defs',
  '$ref',
  'type',
  'properties',
  'required',
  'additionalProperties',
  'enum',
  'const',
  'items',
  'minItems',
  'maxItems',
  'minLength',
  'maxLength',
  'pattern',
  'minimum',
  'maximum',
  'oneOf',
  'anyOf',
  'description',
  // only `"format": "date-time"` (beside a timestamp's pattern; agent-report and AAC both check real calendar dates)
  'format',
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function parseJson(path: string): unknown {
  return JSON.parse(readFileSync(path, 'utf8'));
}

function collectKeywordViolations(node: unknown, path: string, out: string[]): void {
  if (Array.isArray(node)) {
    node.forEach((entry, index) => collectKeywordViolations(entry, `${path}[${index}]`, out));
    return;
  }
  if (!isRecord(node)) {
    return;
  }
  for (const key of Object.keys(node)) {
    if (key === 'properties' || key === '$defs') {
      const group = node[key];
      if (isRecord(group)) {
        for (const name of Object.keys(group)) {
          collectKeywordViolations(group[name], `${path}.${key}.${name}`, out);
        }
      }
      continue;
    }
    if (!ALLOWED_KEYWORDS.has(key)) {
      out.push(`${path}: unknown keyword "${key}"`);
    }
    if (key === 'format' && (node[key] !== 'date-time' || typeof node.pattern !== 'string')) {
      out.push(`${path}: "format" is only "date-time", beside a pattern`);
    }
    collectKeywordViolations(node[key], `${path}.${key}`, out);
  }
}

function collectRefs(node: unknown, out: string[]): void {
  if (Array.isArray(node)) {
    for (const entry of node) {
      collectRefs(entry, out);
    }
    return;
  }
  if (!isRecord(node)) {
    return;
  }
  for (const [key, value] of Object.entries(node)) {
    if (key === '$ref' && typeof value === 'string') {
      out.push(value);
    } else {
      collectRefs(value, out);
    }
  }
}

function resolvePointer(schema: unknown, ref: string): unknown {
  const prefix = '#/$defs/';
  if (!ref.startsWith(prefix)) {
    throw new Error(`$ref "${ref}" does not point inside #/$defs/`);
  }
  const name = ref.slice(prefix.length);
  if (!isRecord(schema) || !isRecord(schema.$defs) || !(name in schema.$defs)) {
    throw new Error(`$ref "${ref}" does not resolve inside its file`);
  }
  return (schema.$defs as Record<string, unknown>)[name];
}

describe('agent-reports schemas', () => {
  for (const kind of KINDS) {
    it(`${kind}.json parses and keeps its schema id`, () => {
      const schema = parseJson(join(schemasDir, `${kind}.json`));
      expect(isRecord(schema)).toBe(true);
      if (!isRecord(schema)) {
        return;
      }
      expect(schema.$schema).toBe('https://json-schema.org/draft/2020-12/schema');
      expect(schema.$id).toBe(`agent-reports/${kind}@1`);
      expect(isRecord(schema.properties)).toBe(true);
      const declared = (schema.properties as Record<string, unknown>).schema;
      expect(isRecord(declared)).toBe(true);
      expect((declared as Record<string, unknown>).const).toBe(`agent-reports/${kind}@1`);
    });
  }

  it('uses only the allowed keywords', () => {
    const violations: string[] = [];
    for (const kind of KINDS) {
      const schema = parseJson(join(schemasDir, `${kind}.json`));
      collectKeywordViolations(schema, `${kind}.json$`, violations);
    }
    expect(violations).toEqual([]);
  });

  it('resolves every $ref inside its own file', () => {
    for (const kind of KINDS) {
      const schema = parseJson(join(schemasDir, `${kind}.json`));
      const refs: string[] = [];
      collectRefs(schema, refs);
      expect(refs.length).toBeGreaterThan(0);
      for (const ref of refs) {
        expect(() => resolvePointer(schema, ref)).not.toThrow();
      }
    }
  });

  it('parses every example file as JSON', () => {
    const files = readdirSync(examplesDir).filter((name) => name.endsWith('.json')).sort();
    expect(files.length).toBeGreaterThan(0);
    for (const kind of KINDS) {
      expect(files).toContain(`${kind}.valid.json`);
    }
    for (const file of files) {
      expect(() => parseJson(join(examplesDir, file))).not.toThrow();
    }
  });
});
