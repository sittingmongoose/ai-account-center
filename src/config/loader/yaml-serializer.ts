/** Serialize active settings; ignored launcher sections are preserved only as raw YAML. */
import * as yaml from 'js-yaml';
import { isDeepStrictEqual } from 'node:util';
import { ConfigError } from '../../errors/error-types';
import type { UnifiedConfig } from '../unified-config-types';

const IGNORED_SECTIONS = new Set([
  'websearch',
  'global_env',
  'continuity',
  'copilot',
  'cursor',
  'thinking',
  'runtime',
  'channels',
  'browser',
  'image_analysis',
]);

export function generateYamlHeader(): string {
  return '# AI Account Center configuration\n# Legacy launcher sections are retained but ignored.\n\n';
}

/** Same options as main's active-section writer: leave alias/reference handling enabled. */
function serialize(config: Record<string, unknown>): string {
  return yaml.dump(config, { indent: 2, lineWidth: -1, quotingType: '"' });
}

/** Compare YAML value graphs without recursively expanding cycles or shared aliases. */
function sameValue(left: unknown, right: unknown): boolean {
  const pending: Array<[unknown, unknown]> = [[left, right]];
  const leftPairs = new WeakMap<object, object>();
  const rightPairs = new WeakMap<object, object>();
  while (pending.length) {
    const [a, b] = pending.pop() as [unknown, unknown];
    if (Object.is(a, b)) continue;
    if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
    if (a instanceof Date || b instanceof Date || ArrayBuffer.isView(a) || ArrayBuffer.isView(b)) {
      if (!isDeepStrictEqual(a, b)) return false;
      continue;
    }
    if (leftPairs.has(a) || rightPairs.has(b)) {
      if (leftPairs.get(a) !== b || rightPairs.get(b) !== a) return false;
      continue;
    }
    leftPairs.set(a, b);
    rightPairs.set(b, a);
    if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
    if (Array.isArray(a) && (!Array.isArray(b) || a.length !== b.length)) return false;
    const keys = Object.keys(a);
    if (keys.length !== Object.keys(b).length) return false;
    for (const key of keys) {
      if (!Object.prototype.hasOwnProperty.call(b, key)) return false;
      pending.push([(a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key]]);
    }
  }
  return true;
}

/** Keep raw retired blocks only when the complete document preserves each section's graph. */
export function generateYamlWithComments(
  config: UnifiedConfig,
  existing = '',
  preserveOriginal?: () => void
): string {
  const active = { ...config };
  const template = { ...config };
  const retired = Object.keys(config).filter((key) => IGNORED_SECTIONS.has(key));
  for (const key of retired) {
    delete active[key];
    template[key] = null; // Positions only: never pass retired values to yaml.dump.
  }
  const generated = serialize(active);
  const expected = yaml.load(generated) as Record<string, unknown>;
  let result = serialize(template);
  const blocks = (text: string): Array<{ key: string; start: number; end: number }> => {
    const keys = [...text.matchAll(/^(?:([A-Za-z_][\w-]*)|'([^']+)'|"([^"\\]+)")\s*:/gm)];
    return keys.map((match, index) => ({
      key: match[1] ?? match[2] ?? match[3],
      start: match.index ?? 0,
      end: keys[index + 1]?.index ?? text.length,
    }));
  };
  const oldBlocks = new Map(blocks(existing).map((block) => [block.key, block]));
  let missing = false;
  for (const block of blocks(result).reverse()) {
    if (!IGNORED_SECTIONS.has(block.key)) continue;
    const old = oldBlocks.get(block.key);
    if (!old) {
      missing = true;
      continue;
    }
    const raw = existing.slice(old.start, old.end);
    result = result.slice(0, block.start) + raw.replace(/\n?$/, '\n') + result.slice(block.end);
  }
  if (config.dashboard_network) {
    result =
      '# Dashboard network: trust_local_network lets plain HTTP from trusted_networks carry\n' +
      '# passwords, keys and sign-in codes (off by default). trusted_networks defaults to\n' +
      '# 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 and fc00::/7.\n' +
      result;
  }
  try {
    const parsed = yaml.load(result) as Record<string, unknown>;
    const keys = [...Object.keys(expected), ...retired];
    if (
      !missing &&
      parsed &&
      typeof parsed === 'object' &&
      Object.keys(parsed).length === keys.length &&
      keys.every(
        (key) =>
          Object.prototype.hasOwnProperty.call(parsed, key) &&
          sameValue(parsed[key], IGNORED_SECTIONS.has(key) ? config[key] : expected[key])
      )
    )
      return result;
  } catch {
    // A marker, unsupported layout or alias crossing section boundaries may invalidate the copy.
  }
  // Never try to encode retired values. Preserve the original before omitting them.
  if (!preserveOriginal)
    throw new ConfigError('Cannot omit retired configuration without backing up the original.');
  preserveOriginal();
  return generated;
}
