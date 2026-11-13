/** Serialize active settings; ignored launcher sections remain opaque. */
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

function serialize(config: UnifiedConfig): { text: string; value: unknown } {
  const text = yaml.dump(config, { indent: 2, lineWidth: -1, quotingType: '"', noRefs: true });
  return { text, value: yaml.load(text) };
}

function matches(text: string, expected: unknown): boolean {
  try {
    return isDeepStrictEqual(yaml.load(text), expected);
  } catch {
    return false;
  }
}

/** Require a durable original backup before omitting unencodable retired data. */
function recoverRetiredSections(config: UnifiedConfig, preserveOriginal?: () => void): string {
  const active = { ...config };
  for (const key of IGNORED_SECTIONS) delete active[key];
  // If active settings cannot be encoded either, throw before any file is changed.
  const recovered = serialize(active);
  if (!preserveOriginal) {
    throw new ConfigError('Cannot omit retired configuration without backing up the original.');
  }
  preserveOriginal();
  return recovered.text;
}

/** Keep raw retired blocks only when the complete output preserves the intended values. */
export function generateYamlWithComments(
  config: UnifiedConfig,
  existing = '',
  preserveOriginal?: () => void
): string {
  let generated: { text: string; value: unknown };
  try {
    generated = serialize(config);
  } catch {
    return recoverRetiredSections(config, preserveOriginal);
  }
  const blocks = (text: string): Array<{ key: string; start: number; end: number }> => {
    const keys = [...text.matchAll(/^(?:([A-Za-z_][\w-]*)|'([^']+)'|"([^"\\]+)")\s*:/gm)];
    return keys.map((match, index) => ({
      key: match[1] ?? match[2] ?? match[3],
      start: match.index ?? 0,
      end: keys[index + 1]?.index ?? text.length,
    }));
  };
  const oldBlocks = new Map(blocks(existing).map((block) => [block.key, block]));
  let result = generated.text;
  for (const block of blocks(generated.text).reverse()) {
    const old = oldBlocks.get(block.key);
    if (!old || !IGNORED_SECTIONS.has(block.key)) continue;
    // Flow-style mappings or YAML anchors can span top-level sections. Only splice
    // a block when it independently parses to exactly its original opaque value.
    const raw = existing.slice(old.start, old.end);
    try {
      const parsed = yaml.load(raw) as Record<string, unknown>;
      if (!isDeepStrictEqual(parsed, { [block.key]: config[block.key] })) continue;
      result = result.slice(0, block.start) + raw.replace(/\n?$/, '\n') + result.slice(block.end);
    } catch {
      // Unusual shared-anchor syntax still round-trips as the parsed YAML value.
    }
  }
  if (config.dashboard_network) {
    result =
      '# Dashboard network: trust_local_network lets plain HTTP from trusted_networks carry\n' +
      '# passwords, keys and sign-in codes (off by default). trusted_networks defaults to\n' +
      '# 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 and fc00::/7.\n' +
      result;
  }
  if (matches(result, generated.value)) return result;
  // A raw block may contain a document marker or otherwise affect later sections.
  // Re-serialize its parsed values when the complete spliced document is unsafe.
  if (matches(generated.text, generated.value)) return generated.text;
  return recoverRetiredSections(config, preserveOriginal);
}
