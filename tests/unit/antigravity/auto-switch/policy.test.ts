import { describe, expect, test } from 'bun:test';
import {
  decideAntigravityAutoSwitch,
  matchingPoolRemaining,
  sameAntigravityDecision,
} from '../../../../src/antigravity/auto-switch/policy';
import {
  defaultAntigravityAutoSwitchState,
  isAntigravityAutoSwitchSettingsPatch,
  mergeAntigravityAutoSwitchSettings,
  validateAntigravityAutoSwitchStoredState,
} from '../../../../src/antigravity/auto-switch/settings';
import type { AntigravityAutoObservation } from '../../../../src/antigravity/auto-switch/types';
import { NOW, copy, futureReset, observation, profile, state } from './fixtures';

describe('independent settings', () => {
  test('defaults are Ubuntu-only, off and explicitly unconfigured', () => {
    const value = defaultAntigravityAutoSwitchState();
    expect(value.settings).toEqual({
      enabled: false,
      thresholdUsedPercent: 95,
      pollIntervalSeconds: 60,
      maxQuotaAgeSeconds: 300,
      cooldownSeconds: 300,
      selectedHostIds: ['ubuntu'],
      requestedPoolId: null,
    });
    value.settings.selectedHostIds.length = 0;
    expect(defaultAntigravityAutoSwitchState().settings.selectedHostIds).toEqual(['ubuntu']);
  });

  const invalidPatches = [
    null,
    [],
    {},
    { unknown: true },
    { enabled: 'true' },
    { thresholdUsedPercent: 0 },
    { thresholdUsedPercent: 100 },
    { thresholdUsedPercent: 95.5 },
    { thresholdUsedPercent: NaN },
    { thresholdPercent: 5 },
    { pollIntervalSeconds: 14 },
    { pollIntervalSeconds: 3601 },
    { maxQuotaAgeSeconds: 14 },
    { maxQuotaAgeSeconds: 901 },
    { cooldownSeconds: 59 },
    { cooldownSeconds: 3601 },
    { selectedHostIds: [] },
    { selectedHostIds: ['mac'] },
    { selectedHostIds: ['ubuntu', 'windows'] },
    { selectedHostIds: ['ubuntu', 'ubuntu'] },
    { requestedPoolId: '' },
    { requestedPoolId: 'some label' },
    { requestedPoolId: 'pool\nsecret' },
  ];
  for (const [index, patch] of invalidPatches.entries())
    test(`reject invalid patch ${index}`, () => {
      expect(isAntigravityAutoSwitchSettingsPatch(patch)).toBe(false);
      expect(() => mergeAntigravityAutoSwitchSettings(state(), patch)).toThrow(
        'Invalid Antigravity'
      );
    });

  test('supports exact model/pool IDs without guessing labels', () => {
    const next = mergeAntigravityAutoSwitchSettings(state(), {
      enabled: true,
      requestedPoolId: 'google/claude-gpt:shared-1',
    });
    expect(next.settings.requestedPoolId).toBe('google/claude-gpt:shared-1');
    expect(state().settings.requestedPoolId).toBe('gemini-pool');
  });

  test('strict stored schema rejects unknown version, missing fields and malformed cooldown', () => {
    for (const value of [
      { ...state(), version: 2 },
      { ...state(), enabled: true },
      { version: 1, settings: { enabled: true }, lastSwitch: null },
      {
        ...state(),
        lastSwitch: { at: 'invalid', hostId: 'ubuntu', profileId: 'party' },
      },
      {
        ...state(),
        lastSwitch: {
          at: new Date(NOW).toISOString(),
          hostId: 'mac',
          profileId: 'party',
        },
      },
    ]) {
      expect(() => validateAntigravityAutoSwitchStoredState(value)).toThrow();
    }
  });
});

describe('fresh exact-pool switching policy', () => {
  test('missing and malformed idle-census flags never count as idle', () => {
    for (const key of ['available', 'complete', 'busy', 'manualActivationInProgress']) {
      for (const replacement of [undefined, 'false', 'true', 0, 1, null]) {
        const value = observation();
        if (replacement === undefined) Reflect.deleteProperty(value.hosts[0], key);
        else Object.assign(value.hosts[0], { [key]: replacement });
        expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('waiting_idle');
      }
    }
  });

  test('missing and malformed verified-profile flags never permit automatic setup', () => {
    for (const key of ['authValid', 'nativeConsumerCompatible', 'isActive']) {
      for (const replacement of [undefined, 'false', 'true', 0, 1, null]) {
        const value = observation();
        if (replacement === undefined) Reflect.deleteProperty(value.profiles[1], key);
        else Object.assign(value.profiles[1], { [key]: replacement });
        expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('setup_required');
      }
    }
  });

  test('unverified active profiles cannot be hidden by filtering eligible candidates', () => {
    const value = observation();
    value.profiles[0].authValid = false;
    value.profiles[1].isActive = true;
    value.profiles.push(profile('third'));
    expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('setup_required');
  });

  test('malformed completeness and window booleans reject selected-pool quota', () => {
    for (const index of [0, 1]) {
      for (const key of ['complete', 'enabled', 'unlimited']) {
        const value = observation();
        Object.assign(
          key === 'complete'
            ? value.quotas[index].pools[0]
            : value.quotas[index].pools[0].windows[0],
          { [key]: 'false' }
        );
        expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe(
          index === 0 ? 'no_fresh_quota' : 'no_candidate'
        );
      }
    }
  });

  test('API-key quota, a foreign host, or a different profile cannot masquerade as native quota', () => {
    for (const index of [0, 1]) {
      for (const patch of [
        { source: 'api-key' },
        { hostId: 'windows' },
        { profileId: 'foreign-profile' },
      ]) {
        const value = observation();
        Object.assign(value.quotas[index], patch);
        expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe(
          index === 0 ? 'no_fresh_quota' : 'no_candidate'
        );
      }
    }
  });

  test('95 percent used triggers the exact five percent remaining boundary', () => {
    const decision = decideAntigravityAutoSwitch(state(), observation(), NOW);
    expect(decision.outcome).toBe('scheduled');
    expect(decision.target?.id).toBe('party');
    expect(decision.activeRemainingPercent).toBe(5);
    expect(decision.targetRemainingPercent).toBe(80);
  });

  test('healthy active account does not switch', () => {
    const value = observation();
    value.quotas[0].pools[0].windows[0].remainingPercent = 5.01;
    expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('healthy');
  });

  test('unrelated exhausted pool and shared credit balance never trigger or rank a decision', () => {
    const value = observation();
    value.quotas[0].pools.push({
      id: 'claude-gpt-pool',
      idSource: 'provider-id',
      eligibility: 'reported-quota',
      complete: true,
      windows: [
        {
          key: 'claude-only',
          kind: 'rate_limit',
          remainingPercent: 0,
          resetAt: futureReset,
        },
      ],
    });
    value.quotas[0].pools[0].windows.push({
      key: 'shared-credits',
      kind: 'balance',
      remainingPercent: 0,
      resetAt: null,
    });
    value.quotas[1].pools[0].windows.push({
      key: 'extra-pack',
      kind: 'extra_usage',
      remainingPercent: 0,
      resetAt: null,
    });
    value.quotas[0].pools[0].windows[0].remainingPercent = 70;
    expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('healthy');
    value.quotas[0].pools[0].windows[0].remainingPercent = 5;
    expect(decideAntigravityAutoSwitch(state(), value, NOW).target?.id).toBe('party');
  });

  test('a provider-reported additional constraint inside the selected pool constrains its health', () => {
    const value = observation();
    value.quotas[1].pools[0].windows.push({
      key: 'reported-weekly',
      kind: 'rate_limit',
      remainingPercent: 4,
      resetAt: new Date(NOW + 4 * 86400_000).toISOString(),
    });
    expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('no_candidate');
  });

  const invalidQuota: Array<[string, (value: AntigravityAutoObservation, index: number) => void]> =
    [
      [
        'cached',
        (value, index) => {
          value.quotas[index].status = 'cached';
        },
      ],
      [
        'rate limited',
        (value, index) => {
          value.quotas[index].status = 'rate_limited';
        },
      ],
      [
        'error',
        (value, index) => {
          value.quotas[index].status = 'error';
        },
      ],
      [
        'unavailable',
        (value, index) => {
          value.quotas[index].status = 'unavailable';
        },
      ],
      [
        'wrong identity',
        (value, index) => {
          value.quotas[index].identityKey = 'another-google-user';
        },
      ],
      [
        'old credential',
        (value, index) => {
          value.quotas[index].credentialRevision = 'foreign-refresh';
        },
      ],
      [
        'stale sample',
        (value, index) => {
          value.quotas[index].sampledAt = new Date(NOW - 300_001).toISOString();
        },
      ],
      [
        'future sample',
        (value, index) => {
          value.quotas[index].sampledAt = new Date(NOW + 1).toISOString();
        },
      ],
      [
        'invalid sample',
        (value, index) => {
          value.quotas[index].sampledAt = 'invalid';
        },
      ],
      [
        'wrong requested pool',
        (value, index) => {
          value.quotas[index].pools[0].id = 'claude-gpt-pool';
        },
      ],
      [
        'incomplete pool',
        (value, index) => {
          value.quotas[index].pools[0].complete = false;
        },
      ],
      [
        'unverified eligibility',
        (value, index) => {
          value.quotas[index].pools[0].eligibility = 'unverified';
        },
      ],
      [
        'missing pool windows',
        (value, index) => {
          value.quotas[index].pools[0].windows = [];
        },
      ],
      [
        'missing reset',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].resetAt = null;
        },
      ],
      [
        'expired reset',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].resetAt = new Date(NOW).toISOString();
        },
      ],
      [
        'invalid reset',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].resetAt = 'invalid';
        },
      ],
      [
        'missing percent',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].remainingPercent = null;
        },
      ],
      [
        'over 100 percent',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].remainingPercent = 101;
        },
      ],
      [
        'negative percent',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].remainingPercent = -1;
        },
      ],
      [
        'nonfinite percent',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].remainingPercent = Infinity;
        },
      ],
      [
        'disabled rate window',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].enabled = false;
        },
      ],
      [
        'unspecified unlimited budget',
        (value, index) => {
          value.quotas[index].pools[0].windows[0].unlimited = true;
        },
      ],
      [
        'duplicate window',
        (value, index) => {
          value.quotas[index].pools[0].windows.push(copy(value.quotas[index].pools[0].windows[0]));
        },
      ],
      [
        'duplicate quota sample',
        (value, index) => {
          value.quotas.push(copy(value.quotas[index]));
        },
      ],
      [
        'duplicate pool',
        (value, index) => {
          value.quotas[index].pools.push(copy(value.quotas[index].pools[0]));
        },
      ],
    ];
  for (const [label, mutate] of invalidQuota)
    for (const index of [0, 1])
      test(`${label} rejects ${index === 0 ? 'active decision' : 'candidate'}`, () => {
        const value = observation();
        mutate(value, index);
        expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe(
          index === 0 ? 'no_fresh_quota' : 'no_candidate'
        );
      });

  test('five-minute age limit is inclusive and provider windows are not invented', () => {
    const value = observation();
    value.quotas[0].sampledAt = new Date(NOW - 300_000).toISOString();
    expect(matchingPoolRemaining(value.profiles[0], value, state(), NOW)).toBe(5);
    expect(value.quotas[0].pools[0].windows).toHaveLength(1);
  });

  test('requested pool can explicitly select Claude/GPT instead of Gemini', () => {
    const config = state();
    config.settings.requestedPoolId = 'claude-gpt-pool';
    const value = observation();
    value.quotas.forEach((quota) => {
      quota.pools[0].id = 'claude-gpt-pool';
    });
    expect(decideAntigravityAutoSwitch(config, value, NOW).target?.id).toBe('party');
  });

  test('authoritative bucket-group membership is accepted without inventing an entitlement flag', () => {
    const config = state();
    config.settings.requestedPoolId = 'bucket-group:actual-membership-fixture';
    const value = observation();
    value.quotas.forEach((quota) => {
      quota.pools[0].id = config.settings.requestedPoolId!;
      quota.pools[0].idSource = 'provider-bucket-membership';
      quota.pools[0].eligibility = 'reported-quota';
      quota.pools[0].windows[0].key = 'gemini-5h';
      quota.pools[0].windows.push({
        key: 'gemini-weekly',
        kind: 'rate_limit',
        remainingPercent: 80,
        resetAt: futureReset,
      });
    });
    expect(decideAntigravityAutoSwitch(config, value, NOW).target?.id).toBe('party');
    value.quotas[1].pools[0].windows[1].remainingPercent = 0;
    expect(decideAntigravityAutoSwitch(config, value, NOW).outcome).toBe('no_candidate');
  });

  test('a candidate at the threshold or without five percent improvement is rejected', () => {
    for (const remaining of [0, 5, 5.01, 9.99]) {
      const value = observation();
      value.quotas[1].pools[0].windows[0].remainingPercent = remaining;
      expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('no_candidate');
    }
    const value = observation();
    value.quotas[1].pools[0].windows[0].remainingPercent = 10;
    expect(decideAntigravityAutoSwitch(state(), value, NOW).target?.id).toBe('party');
  });

  test('ranking is highest matching-pool headroom then stable profile ID, independent of array order', () => {
    const value = observation();
    value.profiles.push(profile('aaa'));
    value.quotas.push({
      ...copy(value.quotas[1]),
      profileId: 'aaa',
      identityKey: 'verified-google-subject:aaa',
      credentialRevision: 'private-revision:aaa',
    });
    expect(decideAntigravityAutoSwitch(state(), value, NOW).target?.id).toBe('aaa');
    value.profiles.reverse();
    value.quotas.reverse();
    expect(decideAntigravityAutoSwitch(state(), value, NOW).target?.id).toBe('aaa');
    value.quotas.find(
      (quota) => quota.profileId === 'party'
    )!.pools[0].windows[0].remainingPercent = 90;
    expect(decideAntigravityAutoSwitch(state(), value, NOW).target?.id).toBe('party');
  });

  const invalidHost: Array<[string, (value: AntigravityAutoObservation) => void]> = [
    [
      'busy CLI',
      (value) => {
        value.hosts[0].busy = true;
      },
    ],
    [
      'manual activation',
      (value) => {
        value.hosts[0].manualActivationInProgress = true;
      },
    ],
    [
      'unavailable host',
      (value) => {
        value.hosts[0].available = false;
      },
    ],
    [
      'unreviewed process census',
      (value) => {
        value.hosts[0].complete = false;
      },
    ],
    [
      'missing census',
      (value) => {
        value.hosts = [];
      },
    ],
    [
      'duplicate census',
      (value) => {
        value.hosts.push(copy(value.hosts[0]));
      },
    ],
    [
      'stale census',
      (value) => {
        value.hosts[0].sampledAt = new Date(NOW - 10_001).toISOString();
      },
    ],
    [
      'future census',
      (value) => {
        value.hosts[0].sampledAt = new Date(NOW + 1).toISOString();
      },
    ],
  ];
  for (const [label, mutate] of invalidHost)
    test(`${label} waits without stop approval`, () => {
      const value = observation();
      mutate(value);
      expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('waiting_idle');
    });

  test('persisted cooldown prevents bounce and survives monitor recreation', () => {
    const config = state();
    config.lastSwitch = {
      at: new Date(NOW - 299_999).toISOString(),
      hostId: 'ubuntu',
      profileId: 'gmail',
    };
    expect(decideAntigravityAutoSwitch(config, observation(), NOW).outcome).toBe('cooldown');
    config.lastSwitch.at = new Date(NOW - 300_000).toISOString();
    expect(decideAntigravityAutoSwitch(config, observation(), NOW).outcome).toBe('scheduled');
    config.lastSwitch.at = new Date(NOW + 1).toISOString();
    expect(decideAntigravityAutoSwitch(config, observation(), NOW).outcome).toBe('error');
  });

  test('two distinct verified native-compatible accounts and one known active account are required', () => {
    for (const mutate of [
      (value: AntigravityAutoObservation) => {
        value.profiles.pop();
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[1].authValid = false;
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[1].nativeConsumerCompatible = false;
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[1].identityKey = value.profiles[0].identityKey;
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[1].id = value.profiles[0].id;
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[0].isActive = false;
      },
      (value: AntigravityAutoObservation) => {
        value.profiles[1].isActive = true;
      },
    ]) {
      const value = observation();
      mutate(value);
      expect(decideAntigravityAutoSwitch(state(), value, NOW).outcome).toBe('setup_required');
    }
  });

  test('revalidation compares host, current/target identity, credential revision and target ranking', () => {
    const original = decideAntigravityAutoSwitch(state(), observation(), NOW);
    expect(
      sameAntigravityDecision(original, decideAntigravityAutoSwitch(state(), observation(), NOW))
    ).toBe(true);
    const changed = observation();
    changed.profiles[1].credentialRevision = 'new-native-credential';
    changed.quotas[1].credentialRevision = 'new-native-credential';
    expect(
      sameAntigravityDecision(original, decideAntigravityAutoSwitch(state(), changed, NOW))
    ).toBe(false);
  });
});
