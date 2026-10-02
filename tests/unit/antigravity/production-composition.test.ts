import { describe, expect, spyOn, test } from 'bun:test';
import {
  createUbuntuAntigravityDriver,
  type UbuntuAntigravityDriverDependencies,
} from '../../../src/antigravity/ubuntu-driver';
import { credentialFingerprint } from '../../../src/antigravity/registry';
import {
  publicDashboardAccount,
  publicProfile,
} from '../../../src/antigravity/usage-normalization';
import type {
  AntigravityDashboardAccount,
  AntigravityUsageProfile,
} from '../../../src/antigravity/usage-contract';
import type {
  InstallReceipt,
  NativeCredential,
  ProcessPlan,
  QuiescedHostProof,
  RuntimeProof,
  StopReceipt,
  VerifiedIdentity,
} from '../../../src/antigravity/types';

// Every byte, identity, PID, receipt and dependency below is invented. This
// suite never constructs a native store, quota worker or IPC transport.
const PRIVATE = 'FIXTURE_ONLY_PRIVATE_NATIVE_SENTINEL';
const NOW = '2026-10-01T20:00:00.000Z';

async function withClock(run: (clock: { now: number }) => Promise<void>) {
  const clock = { now: Date.parse(NOW) };
  const dateNow = spyOn(Date, 'now').mockImplementation(() => clock.now);
  try {
    await run(clock);
  } finally {
    dateNow.mockRestore();
  }
}

function quiescedProof(sampledAt = NOW): QuiescedHostProof {
  return {
    hostId: 'ubuntu',
    available: true,
    complete: true,
    busy: false,
    manualActivationInProgress: false,
    sampledAt,
  };
}

function credential(version = 1): NativeCredential {
  return {
    format: 'fake-native-json',
    bytes: Buffer.from(JSON.stringify({ version, private: PRIVATE })),
  };
}

function identity(source: VerifiedIdentity['source'] = 'provider-userinfo'): VerifiedIdentity {
  return {
    email: 'party@example.com',
    subject: 'fixture-authoritative-party-subject',
    plan: 'Google AI Pro',
    verifiedAt: NOW,
    source,
  };
}

function fixture(includeRelease = true) {
  const calls: string[] = [];
  const state = {
    current: credential(),
    actualIdentity: identity(),
    released: true,
    capable: true,
    releaseError: false,
    proof: {
      identity: identity('native-runtime'),
      credentialFingerprint: credentialFingerprint(credential()),
      runtimeStarted: true,
      sessionRestored: true,
    } as RuntimeProof,
  };
  const plan: ProcessPlan = { complete: true, processes: [], continuity: null };
  const stopped: StopReceipt = {
    complete: true,
    stopped: [],
    restartState: { privateFixtureState: PRIVATE },
  };
  const installed: InstallReceipt = {
    installedFingerprint: credentialFingerprint(credential(2)),
    rollbackState: { privateFixtureState: PRIVATE },
  };
  const argumentsSeen = {
    validated: [] as NativeCredential[],
    stopped: [] as ProcessPlan[],
    installed: [] as Array<[NativeCredential, string]>,
    restarted: [] as StopReceipt[],
    proved: [] as VerifiedIdentity[],
    rolledBack: [] as Array<[InstallReceipt, NativeCredential]>,
  };
  const deps: UbuntuAntigravityDriverDependencies = {
    nativeStore: {
      read: async () => {
        calls.push('native.read');
        return state.current;
      },
      install: async (next, expectedFingerprint) => {
        calls.push('native.install');
        argumentsSeen.installed.push([next, expectedFingerprint]);
        return installed;
      },
      rollback: async (receipt, previous) => {
        calls.push('native.rollback');
        argumentsSeen.rolledBack.push([receipt, previous]);
        return true;
      },
    },
    quotaWorker: {
      validateCredential: async (value) => {
        calls.push('quota.identity');
        argumentsSeen.validated.push(value);
        return state.actualIdentity;
      },
    },
    bridge: {
      readHostCensus: async () => {
        calls.push('bridge.census');
        return {
          hostId: 'ubuntu',
          available: false,
          complete: false,
          busy: true,
          manualActivationInProgress: false,
          sampledAt: '',
        };
      },
      canProveRuntimeIdentity: async () => {
        calls.push('bridge.capability');
        return state.capable;
      },
      inspectProcesses: async () => {
        calls.push('bridge.inspect');
        return plan;
      },
      stopProcesses: async (value) => {
        calls.push('bridge.stop');
        argumentsSeen.stopped.push(value);
        return stopped;
      },
      restartProcesses: async (value) => {
        calls.push('bridge.restart');
        argumentsSeen.restarted.push(value);
      },
      proveRuntimeIdentity: async (expected) => {
        calls.push('bridge.prove');
        argumentsSeen.proved.push(expected);
        return state.proof;
      },
      stopOwnedRestarts: async () => {
        calls.push('bridge.stop-owned');
      },
    },
    ...(includeRelease
      ? {
          releaseGate: async () => {
            calls.push('release');
            if (state.releaseError) throw new Error(PRIVATE);
            return state.released;
          },
        }
      : {}),
  };
  return {
    calls,
    state,
    deps,
    argumentsSeen,
    plan,
    stopped,
    installed,
    construct: () => createUbuntuAntigravityDriver(deps),
  };
}

describe('explicit Ubuntu Antigravity production-driver composition', () => {
  test('construction performs no injected native, quota, IPC, release or process work', () => {
    const f = fixture();
    const driver = f.construct();
    expect(driver.hostId).toBe('ubuntu');
    expect(f.calls).toEqual([]);
    for (const method of [
      'canProveRuntimeIdentity',
      'readCurrentCredential',
      'validateCredential',
      'inspectProcesses',
      'stopProcesses',
      'installCredential',
      'readStoredIdentity',
      'restartProcesses',
      'proveRuntimeIdentity',
      'rollbackCredential',
      'stopOwnedRestarts',
      'approveOwnedIdlePlan',
      'revalidateQuiescedStop',
      'completeActivation',
      'canRestartSupportedNative',
    ] as const) {
      expect(typeof driver[method]).toBe('function');
    }
    expect(f.argumentsSeen).toEqual({
      validated: [],
      stopped: [],
      installed: [],
      restarted: [],
      proved: [],
      rolledBack: [],
    });
  });

  test('absent release defaults false without consulting the broker', async () => {
    const f = fixture(false);
    expect(await f.construct().canProveRuntimeIdentity()).toBe(false);
    expect(f.calls).toEqual([]);
  });

  test.each(['stop', 'install', 'prove'] as const)(
    'absent release blocks %s before any dependency action',
    async (operation) => {
      const f = fixture(false);
      const driver = f.construct();
      const action =
        operation === 'stop'
          ? driver.stopProcesses(f.plan)
          : operation === 'install'
            ? driver.installCredential(credential(2), credentialFingerprint(f.state.current))
            : driver.proveRuntimeIdentity(identity());
      await expect(action).rejects.toThrow('antigravity-runtime-unavailable');
      expect(f.calls).toEqual([]);
    }
  );

  test.each([false, null, undefined, 1, 'true'])(
    'release must be exactly true: %s never reaches the broker',
    async (value) => {
      const f = fixture();
      f.deps.releaseGate = async () => {
        f.calls.push('release');
        return value as boolean;
      };
      expect(await f.construct().canProveRuntimeIdentity()).toBe(false);
      expect(f.calls).toEqual(['release']);
    }
  );

  test('release failure is closed and never exposes its private exception', async () => {
    const f = fixture();
    f.state.releaseError = true;
    const driver = f.construct();
    expect(await driver.canProveRuntimeIdentity()).toBe(false);
    await expect(driver.stopProcesses(f.plan)).rejects.toThrow('antigravity-runtime-unavailable');
    expect(f.calls).toEqual(['release', 'release']);
  });

  test('broker capability failure closes each mutation and proof without leaking its exception', async () => {
    const f = fixture();
    f.deps.bridge.canProveRuntimeIdentity = async () => {
      f.calls.push('bridge.capability');
      throw new Error(PRIVATE);
    };
    const driver = f.construct();
    expect(await driver.canProveRuntimeIdentity()).toBe(false);
    await expect(driver.stopProcesses(f.plan)).rejects.toThrow('antigravity-runtime-unavailable');
    await expect(
      driver.installCredential(credential(2), credentialFingerprint(f.state.current))
    ).rejects.toThrow('antigravity-runtime-unavailable');
    await expect(driver.proveRuntimeIdentity(identity())).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.calls).toEqual([
      'release',
      'bridge.capability',
      'release',
      'bridge.capability',
      'release',
      'bridge.capability',
      'release',
      'bridge.capability',
    ]);
    expect(f.argumentsSeen.installed).toEqual([]);
    expect(f.argumentsSeen.stopped).toEqual([]);
    expect(f.argumentsSeen.proved).toEqual([]);
  });

  test.each([false, null, undefined, 1, 'true'])(
    'a released driver still needs broker capability exactly true: %s',
    async (value) => {
      const f = fixture();
      f.deps.bridge.canProveRuntimeIdentity = async () => {
        f.calls.push('bridge.capability');
        return value as boolean;
      };
      const driver = f.construct();
      expect(await driver.canProveRuntimeIdentity()).toBe(false);
      await expect(
        driver.installCredential(credential(2), credentialFingerprint(f.state.current))
      ).rejects.toThrow('antigravity-runtime-unavailable');
      expect(f.argumentsSeen.installed).toEqual([]);
      expect(f.calls).toEqual(['release', 'bridge.capability', 'release', 'bridge.capability']);
    }
  );

  test('standalone install and stop freshly consult release and broker capability', async () => {
    const f = fixture();
    const driver = f.construct();
    expect(await driver.canProveRuntimeIdentity()).toBe(true);
    const next = credential(2);
    const currentFingerprint = credentialFingerprint(f.state.current);
    expect(await driver.installCredential(next, currentFingerprint)).toBe(f.installed);
    expect(await driver.stopProcesses(f.plan)).toBe(f.stopped);
    expect(f.argumentsSeen.stopped).toEqual([f.plan]);
    expect(f.argumentsSeen.installed).toEqual([[next, currentFingerprint]]);
    expect(f.calls).toEqual([
      'release',
      'bridge.capability',
      'release',
      'bridge.capability',
      'native.install',
      'release',
      'bridge.capability',
      'bridge.stop',
    ]);
    f.calls.length = 0;
    f.state.released = false;
    await expect(driver.stopProcesses(f.plan)).rejects.toThrow('antigravity-runtime-unavailable');
    await expect(f.construct().installCredential(next, currentFingerprint)).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.calls).toEqual(['release', 'release']);
    expect(f.argumentsSeen.stopped).toHaveLength(1);
    expect(f.argumentsSeen.installed).toHaveLength(1);
  });

  test('optional native approvals fail closed when the source release is absent', async () => {
    const f = fixture(false);
    const driver = f.construct();
    expect(await driver.canRestartSupportedNative!()).toBe(false);
    expect(
      await driver.approveOwnedIdlePlan!(f.plan, identity(), credentialFingerprint(f.state.current))
    ).toBe(false);
    expect(
      await driver.revalidateQuiescedStop!(
        f.stopped,
        identity(),
        credentialFingerprint(f.state.current)
      )
    ).toBeNull();
    await expect(driver.completeActivation!(identity())).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.calls).toEqual([]);
  });

  test('legacy optional bridge methods cannot earn idle approval or a stopped lease', async () => {
    const f = fixture();
    const driver = f.construct();
    expect(
      await driver.approveOwnedIdlePlan!(f.plan, identity(), credentialFingerprint(f.state.current))
    ).toBe(false);
    expect(
      await driver.revalidateQuiescedStop!(
        f.stopped,
        identity(),
        credentialFingerprint(f.state.current)
      )
    ).toBeNull();
    await expect(driver.completeActivation!(identity())).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.calls).toEqual(['release', 'bridge.capability', 'release']);
    expect(f.argumentsSeen.installed).toEqual([]);
  });

  test('restart source support remains true while stopped broker runtime capability is false', async () => {
    const f = fixture();
    f.state.capable = false;
    const driver = f.construct();
    expect(await driver.canRestartSupportedNative!()).toBe(true);
    expect(f.calls).toEqual(['release']);
    expect(await driver.canProveRuntimeIdentity()).toBe(false);
    f.state.released = false;
    expect(await driver.canRestartSupportedNative!()).toBe(false);
    expect(f.calls).toEqual(['release', 'release', 'bridge.capability', 'release']);
  });

  test('idle approval binds the exact plan, verified identity and credential fingerprint', async () => {
    const f = fixture();
    const expected = identity();
    const fingerprint = credentialFingerprint(f.state.current);
    const seen: unknown[][] = [];
    f.deps.bridge.approveOwnedIdlePlan = async (...args) => {
      f.calls.push('bridge.approve-idle');
      seen.push(args);
      return true;
    };
    const driver = f.construct();
    expect(await driver.approveOwnedIdlePlan!(f.plan, expected, fingerprint)).toBe(true);
    expect(seen).toEqual([[f.plan, expected, fingerprint]]);
    expect(f.calls).toEqual(['release', 'bridge.capability', 'bridge.approve-idle']);
    f.state.capable = false;
    expect(await driver.approveOwnedIdlePlan!(f.plan, expected, fingerprint)).toBe(false);
    expect(seen).toHaveLength(1);
  });

  test('an owned stopped driver requires an earned lease even if static and live gates still pass', async () => {
    const f = fixture();
    const driver = f.construct();
    await driver.stopProcesses(f.plan);
    f.calls.length = 0;
    await expect(
      driver.installCredential(credential(2), credentialFingerprint(f.state.current))
    ).rejects.toThrow('antigravity-runtime-unavailable');
    expect(f.calls).toEqual([]);
    expect(f.argumentsSeen.installed).toEqual([]);
  });

  test('fresh stopped lease permits one install while runtime capability is false', async () => {
    await withClock(async (clock) => {
      const f = fixture();
      const driver = f.construct();
      const fingerprint = credentialFingerprint(f.state.current);
      const expected = identity();
      const proof = quiescedProof(new Date(clock.now).toISOString());
      const seen: unknown[][] = [];
      f.deps.bridge.revalidateQuiescedStop = async (...args) => {
        f.calls.push('bridge.quiesced');
        seen.push(args);
        return proof;
      };
      await driver.stopProcesses(f.plan);
      f.state.capable = false;
      f.calls.length = 0;
      expect(await driver.revalidateQuiescedStop!(f.stopped, expected, fingerprint)).toBe(proof);
      clock.now += 1000;
      const next = credential(2);
      expect(await driver.installCredential(next, fingerprint)).toBe(f.installed);
      expect(seen).toEqual([
        [f.stopped, expected, fingerprint],
        [f.stopped, expected, fingerprint],
      ]);
      expect(f.calls).toEqual([
        'release',
        'bridge.quiesced',
        'release',
        'bridge.quiesced',
        'native.install',
      ]);
      await expect(driver.installCredential(next, fingerprint)).rejects.toThrow(
        'antigravity-runtime-unavailable'
      );
      expect(f.argumentsSeen.installed).toEqual([[next, fingerprint]]);
    });
  });

  test.each([-1001, 5001, Number.NaN])(
    'stopped lease rejects invalid sample age %s with no static fallback',
    async (age) => {
      await withClock(async (clock) => {
        const f = fixture();
        const driver = f.construct();
        const fingerprint = credentialFingerprint(f.state.current);
        f.deps.bridge.revalidateQuiescedStop = async () => {
          f.calls.push('bridge.quiesced');
          return quiescedProof(
            Number.isFinite(age) ? new Date(clock.now - age).toISOString() : 'invalid-fixture-date'
          );
        };
        await driver.stopProcesses(f.plan);
        expect(await driver.revalidateQuiescedStop!(f.stopped, identity(), fingerprint)).toBeNull();
        f.calls.length = 0;
        await expect(driver.installCredential(credential(2), fingerprint)).rejects.toThrow(
          'antigravity-runtime-unavailable'
        );
        expect(f.calls).toEqual([]);
        expect(f.argumentsSeen.installed).toEqual([]);
      });
    }
  );

  test.each(['expired', 'wrong-fingerprint'] as const)(
    'earned stopped lease rejects %s and is consumed without fallback',
    async (invalid) => {
      await withClock(async (clock) => {
        const f = fixture();
        const driver = f.construct();
        const fingerprint = credentialFingerprint(f.state.current);
        f.deps.bridge.revalidateQuiescedStop = async () => quiescedProof();
        await driver.stopProcesses(f.plan);
        expect(
          await driver.revalidateQuiescedStop!(f.stopped, identity(), fingerprint)
        ).not.toBeNull();
        if (invalid === 'expired') clock.now += 5001;
        f.calls.length = 0;
        await expect(
          driver.installCredential(
            credential(2),
            invalid === 'wrong-fingerprint' ? credentialFingerprint(credential(9)) : fingerprint
          )
        ).rejects.toThrow('antigravity-runtime-unavailable');
        await expect(driver.installCredential(credential(2), fingerprint)).rejects.toThrow(
          'antigravity-runtime-unavailable'
        );
        expect(f.calls).toEqual([]);
        expect(f.argumentsSeen.installed).toEqual([]);
      });
    }
  );

  test.each(['late-refusal', 'wrong-generation'] as const)(
    'stopped install revalidates the exact private lease and refuses %s',
    async (failure) => {
      await withClock(async () => {
        const f = fixture();
        const driver = f.construct();
        const fingerprint = credentialFingerprint(f.state.current);
        const expected = identity();
        const seen: unknown[][] = [];
        let generation = 1;
        let validationCount = 0;
        f.stopped.restartState = { privateFixtureState: PRIVATE, fixtureGeneration: 1 };
        f.deps.bridge.revalidateQuiescedStop = async (...args) => {
          f.calls.push('bridge.quiesced');
          seen.push(args);
          validationCount += 1;
          const receiptGeneration = (args[0].restartState as { fixtureGeneration: number })
            .fixtureGeneration;
          if (
            (failure === 'late-refusal' && validationCount > 1) ||
            receiptGeneration !== generation
          )
            return null;
          return quiescedProof();
        };
        await driver.stopProcesses(f.plan);
        expect(
          await driver.revalidateQuiescedStop!(f.stopped, expected, fingerprint)
        ).not.toBeNull();
        if (failure === 'wrong-generation') generation += 1;
        f.calls.length = 0;
        await expect(driver.installCredential(credential(2), fingerprint)).rejects.toThrow(
          'antigravity-runtime-unavailable'
        );
        expect(seen).toEqual([
          [f.stopped, expected, fingerprint],
          [f.stopped, expected, fingerprint],
        ]);
        expect(f.calls).toEqual(['release', 'bridge.quiesced']);
        expect(f.argumentsSeen.installed).toEqual([]);
      });
    }
  );

  test('completed activation delegates exact identity and clears stopped approval state', async () => {
    await withClock(async () => {
      const f = fixture();
      const driver = f.construct();
      const expected = identity();
      const fingerprint = credentialFingerprint(f.state.current);
      const seen: VerifiedIdentity[] = [];
      f.deps.bridge.revalidateQuiescedStop = async () => quiescedProof();
      f.deps.bridge.completeActivation = async (value) => {
        f.calls.push('bridge.complete');
        seen.push(value);
      };
      await driver.stopProcesses(f.plan);
      await driver.revalidateQuiescedStop!(f.stopped, expected, fingerprint);
      await driver.completeActivation!(expected);
      f.state.capable = false;
      f.calls.length = 0;
      await expect(driver.installCredential(credential(2), fingerprint)).rejects.toThrow(
        'antigravity-runtime-unavailable'
      );
      expect(seen).toEqual([expected]);
      expect(f.calls).toEqual(['release', 'bridge.capability']);
      expect(f.argumentsSeen.installed).toEqual([]);
    });
  });

  test('read and validate delegate private credentials to the authoritative quota worker', async () => {
    const f = fixture(false);
    const driver = f.construct();
    expect(await driver.readCurrentCredential()).toBe(f.state.current);
    const inactive = credential(9);
    expect(await driver.validateCredential(inactive)).toBe(f.state.actualIdentity);
    expect(f.argumentsSeen.validated).toEqual([inactive]);
    expect(f.calls).toEqual(['native.read', 'quota.identity']);
  });

  test('stored identity is freshly read and validated instead of decoded or cached', async () => {
    const f = fixture(false);
    const driver = f.construct();
    expect(await driver.readStoredIdentity()).toBe(f.state.actualIdentity);
    f.state.current = credential(3);
    f.state.actualIdentity = {
      ...identity(),
      email: 'gmail@example.com',
      subject: 'gmail-subject',
    };
    expect(await driver.readStoredIdentity()).toBe(f.state.actualIdentity);
    expect(f.argumentsSeen.validated).toHaveLength(2);
    expect(f.argumentsSeen.validated[1]).toBe(f.state.current);
    expect(f.calls).toEqual(['native.read', 'quota.identity', 'native.read', 'quota.identity']);
  });

  test('inspection is read-only and delegates the broker census without a release gate', async () => {
    const f = fixture(false);
    expect(await f.construct().inspectProcesses()).toBe(f.plan);
    expect(f.calls).toEqual(['bridge.inspect']);
  });

  test('successful runtime proof binds a fresh native read to userinfo and exact bytes', async () => {
    const f = fixture();
    const expected = identity();
    expect(await f.construct().proveRuntimeIdentity(expected)).toBe(f.state.proof);
    expect(f.argumentsSeen.proved).toEqual([expected]);
    expect(f.argumentsSeen.validated).toEqual([f.state.current]);
    expect(f.calls).toEqual([
      'release',
      'bridge.capability',
      'bridge.prove',
      'native.read',
      'quota.identity',
    ]);
  });

  test('a matching native refresh during proof is rebound to its new exact bytes', async () => {
    const f = fixture();
    f.deps.bridge.proveRuntimeIdentity = async () => {
      f.calls.push('bridge.prove');
      f.state.current = credential(4);
      f.state.proof.credentialFingerprint = credentialFingerprint(f.state.current);
      return f.state.proof;
    };
    expect(await f.construct().proveRuntimeIdentity(identity())).toBe(f.state.proof);
    expect(f.argumentsSeen.validated[0]).toBe(f.state.current);
    expect(f.calls.indexOf('native.read')).toBeGreaterThan(f.calls.indexOf('bridge.prove'));
  });

  test('a stale proof for pre-refresh native bytes is rejected', async () => {
    const f = fixture();
    f.deps.bridge.proveRuntimeIdentity = async () => {
      f.calls.push('bridge.prove');
      f.state.current = credential(4);
      return f.state.proof;
    };
    await expect(f.construct().proveRuntimeIdentity(identity())).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.argumentsSeen.validated[0]).toBe(f.state.current);
    expect(f.calls).not.toContain('native.install');
  });

  test.each([
    'actual-email',
    'actual-subject',
    'proof-email',
    'proof-subject',
    'proof-fingerprint',
    'runtime-false',
    'session-false',
    'runtime-missing',
    'session-missing',
  ])('runtime proof rejects %s with a fixed public-safe mismatch error', async (mismatch) => {
    const f = fixture();
    if (mismatch === 'actual-email') f.state.actualIdentity.email = 'foreign@example.com';
    if (mismatch === 'actual-subject') f.state.actualIdentity.subject = PRIVATE;
    if (mismatch === 'proof-email') f.state.proof.identity.email = 'foreign@example.com';
    if (mismatch === 'proof-subject') f.state.proof.identity.subject = PRIVATE;
    if (mismatch === 'proof-fingerprint')
      f.state.proof.credentialFingerprint = credentialFingerprint(credential(100));
    if (mismatch === 'runtime-false') f.state.proof.runtimeStarted = false;
    if (mismatch === 'session-false') f.state.proof.sessionRestored = false;
    if (mismatch === 'runtime-missing')
      f.state.proof.runtimeStarted = undefined as unknown as boolean;
    if (mismatch === 'session-missing')
      f.state.proof.sessionRestored = undefined as unknown as boolean;
    const error = await f
      .construct()
      .proveRuntimeIdentity(identity())
      .catch((value: unknown) => value);
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toBe('antigravity-runtime-unavailable');
    expect((error as Error).message).not.toContain(PRIVATE);
    expect(f.calls).not.toContain('bridge.stop');
    expect(f.calls).not.toContain('native.install');
  });

  test('rollback, owned-stop and restoring restart remain available after release closes', async () => {
    const f = fixture();
    const driver = f.construct();
    expect(await driver.canProveRuntimeIdentity()).toBe(true);
    f.state.released = false;
    f.state.capable = false;
    f.calls.length = 0;
    const previous = credential(7);
    expect(await driver.rollbackCredential(f.installed, previous)).toBe(true);
    await driver.stopOwnedRestarts();
    await driver.restartProcesses(f.stopped);
    expect(f.argumentsSeen.rolledBack).toEqual([[f.installed, previous]]);
    expect(f.argumentsSeen.restarted).toEqual([f.stopped]);
    expect(f.calls).toEqual(['native.rollback', 'bridge.stop-owned', 'bridge.restart']);
  });

  test('public profile and account projections omit all composed native/private extras', () => {
    const privateExtras = {
      credential: credential(),
      subject: PRIVATE,
      nativePath: `/fixture/private/${PRIVATE}`,
      rollbackState: { private: PRIVATE },
      restartState: { private: PRIVATE },
    };
    const profile: AntigravityUsageProfile & typeof privateExtras = {
      id: 'party',
      email: 'party@example.com',
      plan: 'Google AI Pro',
      identityKey: PRIVATE,
      credentialRevision: PRIVATE,
      identityVerified: true,
      available: true,
      selected: true,
      runtimeVerified: true,
      verifiedAt: NOW,
      ...privateExtras,
    };
    const account: AntigravityDashboardAccount & typeof privateExtras = {
      id: 'antigravity:profile:party',
      provider: 'antigravity',
      providerLabel: 'Antigravity',
      label: 'party@example.com',
      email: 'party@example.com',
      plan: 'Google AI Pro',
      platform: 'ubuntu',
      source: 'Antigravity saved login on Ubuntu',
      status: 'ok',
      message: null,
      fetchedAt: NOW,
      sampledAt: NOW,
      isActive: true,
      windows: [],
      capabilities: {
        codexProfile: null,
        claudeProfileId: null,
        claudePlatforms: [],
        antigravityProfileId: 'party',
        antigravityHostIds: ['ubuntu'],
        antigravityCanActivate: true,
        ...privateExtras,
      },
      ...privateExtras,
    };
    const publishedProfile = publicProfile(profile);
    const publishedAccount = publicDashboardAccount(account);
    const serialized = JSON.stringify([publishedProfile, publishedAccount]);
    expect(publishedProfile).toMatchObject({ id: 'party', selected: true, hostId: 'ubuntu' });
    expect(publishedAccount.capabilities.antigravityCanActivate).toBe(true);
    expect(serialized).not.toContain(PRIVATE);
    for (const key of ['credential', 'subject', 'nativePath', 'rollbackState', 'restartState']) {
      expect(publishedProfile).not.toHaveProperty(key);
      expect(publishedAccount).not.toHaveProperty(key);
      expect(publishedAccount.capabilities).not.toHaveProperty(key);
    }
  });
});
