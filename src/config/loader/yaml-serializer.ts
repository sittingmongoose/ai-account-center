/** Serialize active settings; ignored launcher sections remain opaque. */
import * as yaml from 'js-yaml';
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

/** Keep ignored top-level blocks, including comments and scalars, on unrelated saves. */
export function generateYamlWithComments(config: UnifiedConfig, existing = ''): string {
  const generated = yaml.dump(config, { indent: 2, lineWidth: -1, quotingType: '"', noRefs: true });
  const blocks = (text: string): Array<{ key: string; start: number; end: number }> => {
    const keys = [...text.matchAll(/^(?:([A-Za-z_][\w-]*)|'([^']+)'|"([^"\\]+)")\s*:/gm)];
    return keys.map((match, index) => ({
      key: match[1] ?? match[2] ?? match[3],
      start: match.index ?? 0,
      end: keys[index + 1]?.index ?? text.length,
    }));
  };
  const oldBlocks = new Map(blocks(existing).map((block) => [block.key, block]));
  let result = generated;
  for (const block of blocks(generated).reverse()) {
    const old = oldBlocks.get(block.key);
    if (!old || !IGNORED_SECTIONS.has(block.key)) continue;
    // Flow-style mappings or YAML anchors can span top-level sections. Only splice
    // a block when it independently parses to exactly its original opaque value.
    const raw = existing.slice(old.start, old.end);
    try {
      const parsed = yaml.load(raw) as Record<string, unknown>;
      if (JSON.stringify(parsed) !== JSON.stringify({ [block.key]: config[block.key] })) continue;
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
  return result;
}
