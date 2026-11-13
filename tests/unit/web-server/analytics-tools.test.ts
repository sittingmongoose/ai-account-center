import { describe, expect, it } from 'bun:test';
import {
  ANALYTICS_TOOLS,
  EXPERIMENT_ANALYTICS_TOOLS,
  isAnalyticsTool,
  isActivityKind,
} from '../../../src/web-server/usage/analytics-tools';
import { analyticsRemoteTargets } from '../../../src/web-server/services/analytics-remote-sources';
import { DASHBOARD_HOSTS, helperPlatform } from '../../../src/web-server/services/dashboard-hosts';

describe('Analytics vocabulary compatibility', () => {
  it('keeps every existing tool wire value and remote target in order', () => {
    expect(ANALYTICS_TOOLS).toEqual(['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity']);
    for (const host of DASHBOARD_HOSTS.filter((value) => value !== 'ubuntu'))
      expect(analyticsRemoteTargets(host)).toEqual(ANALYTICS_TOOLS);
    expect(EXPERIMENT_ANALYTICS_TOOLS).toEqual(['claude', 'codex', 'muse', 'zcode']);
  });
  it('keeps generic collection work separate from tools and refuses inherited names', () => {
    for (const value of ['jsonl', 'droid', 'experiment-roots', 'toString', '__proto__', 'unknown'])
      expect(isAnalyticsTool(value)).toBe(false);
    expect(isActivityKind('jsonl')).toBe(true);
    expect(isActivityKind('experiment-roots')).toBe(false);
  });
  it('derives the persisted platform from each registered host without adding DTO values', () => {
    expect(helperPlatform('ubuntu')).toBe('ubuntu');
    expect(helperPlatform('mac')).toBe('mac');
    expect(helperPlatform('windows')).toBe('windows');
    expect(helperPlatform('nas1')).toBe('ubuntu');
  });
});
