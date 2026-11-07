import { AntigravityError } from './errors';
import type { AntigravitySwitchDriver, StopReceipt } from './types';
import { checkedPlan, planFingerprint } from './confirmation';
import { AntigravityProfileRegistry, credentialFingerprint, identityKey } from './registry';
import type { RuntimeSocketState } from './runtime-socket-probe';

export interface ManagedNativeUpdate {
  version(): Promise<string>;
  backup(): Promise<void>;
  update(): Promise<void>;
  /** Refuses every native executable replacement not owned by this update. */
  rollback(): Promise<boolean>;
}

export type NativeReviewState = 'reviewed' | 'unreviewed' | 'unknown';

export interface ManagedUpdateResult {
  status: 'current' | 'updated' | 'failed' | 'restart_failed' | 'in_use';
  previousVersion: string;
  version: string;
  restartedProcesses: number;
  updateAttempted: boolean;
  /** Set only by the paused-runtime path (updatePausedAntigravity). */
  runtimePaused?: true;
}

/**
 * Uses the account lock and the same proved resident PTY as account switching.
 * idleOnly never stops a running Antigravity process: the caller sets it when
 * the build `agy update` installs is not reviewed, because the runtime cannot
 * restart a managed session on an unreviewed build. A running process then
 * answers in_use and nothing changes.
 */
export async function updateManagedAntigravity(deps: {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  native: ManagedNativeUpdate;
  idleOnly?: boolean;
  now?: () => number;
}): Promise<ManagedUpdateResult> {
  const { driver, registry, native } = deps;
  const before = await native.version();
  return registry.withLock(async () => {
    const failed = {
      status: 'failed' as const,
      previousVersion: before,
      version: before,
      restartedProcesses: 0,
      updateAttempted: false,
    };
    if (registry.hasRecovery()) return failed;
    const plan = checkedPlan(await driver.inspectProcesses());
    if (!plan.complete) return failed;
    if (!plan.processes.length) {
      let attempted = false;
      try {
        await native.backup();
        const latest = checkedPlan(await driver.inspectProcesses());
        if (
          !latest.complete ||
          latest.processes.length ||
          planFingerprint(latest) !== planFingerprint(plan)
        )
          return failed;
        attempted = true;
        await native.update();
        const after = await native.version();
        // No runtime was stopped or restarted. A changed native pin disables
        // managed launch/switching; ordinary native commands keep working.
        return {
          status: after === before ? 'current' : 'updated',
          previousVersion: before,
          version: after,
          restartedProcesses: 0,
          updateAttempted: true,
        };
      } catch {
        return { ...failed, updateAttempted: attempted };
      }
    }
    if (deps.idleOnly) return { ...failed, status: 'in_use' as const };
    if (
      !(await driver.canProveRuntimeIdentity()) ||
      !driver.approveOwnedIdlePlan ||
      !driver.revalidateQuiescedStop ||
      !driver.completeActivation ||
      !driver.canRestartSupportedNative
    )
      return failed;
    const credential = await driver.readCurrentCredential();
    const current = await driver.validateCredential(credential);
    const id = registry.findIdentity(current);
    if (!id) return failed;
    if (
      !plan.continuity?.restorable ||
      !(await driver.approveOwnedIdlePlan(plan, current, credentialFingerprint(credential)))
    )
      return failed;
    let receipt: StopReceipt | undefined;
    let updated = false;
    let durable = false;
    let stopAttempted = false;
    registry.beginTransaction(id, id, (deps.now ?? Date.now)());
    try {
      await native.backup();
      const latest = checkedPlan(await driver.inspectProcesses());
      if (
        planFingerprint(latest) !== planFingerprint(plan) ||
        !(await driver.approveOwnedIdlePlan(latest, current, credentialFingerprint(credential)))
      )
        throw new AntigravityError('update-deferred');
      stopAttempted = true;
      receipt = await driver.stopProcesses(latest);
      if (
        !receipt.complete ||
        JSON.stringify(receipt.stopped.map((x) => JSON.stringify(x)).sort()) !==
          JSON.stringify(latest.processes.map((x) => JSON.stringify(x.identity)).sort())
      )
        throw new AntigravityError('update-stop-incomplete');
      const stoppedCredential = await driver.readCurrentCredential();
      if (
        identityKey(await driver.validateCredential(stoppedCredential)) !== identityKey(current) ||
        !(await driver.revalidateQuiescedStop(
          receipt,
          current,
          credentialFingerprint(stoppedCredential)
        ))
      )
        throw new AntigravityError('update-stop-unproved');
      updated = true;
      await native.update();
      const after = await native.version();
      // A changed binary invalidates its old proof/pin. Without verifiable
      // publication ownership, an unsupported update retains recovery instead
      // of overwriting the executable or claiming the old PTY was restored.
      if (!(await driver.canRestartSupportedNative()))
        throw new AntigravityError('update-native-proof-pending');
      await driver.restartProcesses(receipt);
      const proof = await driver.proveRuntimeIdentity(current);
      const live = await driver.readCurrentCredential();
      if (
        !proof.runtimeStarted ||
        !proof.sessionRestored ||
        identityKey(proof.identity) !== identityKey(current) ||
        proof.credentialFingerprint !== credentialFingerprint(live) ||
        identityKey(await driver.validateCredential(live)) !== identityKey(current)
      )
        throw new AntigravityError('update-runtime-unproved');
      registry.completeTransaction(id, proof.identity.verifiedAt);
      durable = true;
      await driver.completeActivation(current);
      return {
        status: after === before ? 'current' : 'updated',
        previousVersion: before,
        version: after,
        restartedProcesses: plan.processes.length,
        updateAttempted: true,
      };
    } catch {
      if (!stopAttempted) {
        registry.abortTransaction();
        return failed;
      }
      if (durable) {
        try {
          registry.beginTransaction(id, id, (deps.now ?? Date.now)());
          registry.markRecovery();
        } catch {
          /* Hold driver state. */
        }
        return { ...failed, status: 'restart_failed', updateAttempted: updated };
      }
      try {
        await driver.stopOwnedRestarts();
        if (updated && !(await native.rollback()))
          throw new AntigravityError('update-foreign-native');
        if (!receipt?.complete) throw new AntigravityError('update-stop-unavailable');
        await driver.restartProcesses(receipt);
        const proof = await driver.proveRuntimeIdentity(current);
        const live = await driver.readCurrentCredential();
        if (
          !proof.runtimeStarted ||
          !proof.sessionRestored ||
          identityKey(proof.identity) !== identityKey(current) ||
          proof.credentialFingerprint !== credentialFingerprint(live) ||
          identityKey(await driver.validateCredential(live)) !== identityKey(current)
        )
          throw new AntigravityError('update-recovery-unproved');
        registry.completeTransaction(id, proof.identity.verifiedAt);
        durable = true;
        await driver.completeActivation(current);
        return { ...failed, updateAttempted: updated };
      } catch {
        try {
          if (durable) registry.beginTransaction(id, id, (deps.now ?? Date.now)());
          registry.markRecovery();
        } catch {
          /* Existing intent/driver state remains blocked. */
        }
        return { ...failed, status: 'restart_failed', updateAttempted: updated };
      }
    }
  });
}

/**
 * The managed update while switching is paused: the installed native build
 * has no switching review, so the resident runtime refuses to start and holds
 * no managed session. Like Antigravity's own updater, it replaces the native
 * without checking for running copies; those keep their old binary until restart.
 * The native is backed up and `agy update` runs under the account lock.
 * Failures carry runtimePaused, so the row can say why.
 */
export async function updatePausedAntigravity(deps: {
  registry: AntigravityProfileRegistry;
  native: ManagedNativeUpdate;
  /** True only while the runtime certainly is not running and the build is still unreviewed. */
  stillPaused: () => Promise<boolean>;
}): Promise<ManagedUpdateResult> {
  const { registry, native } = deps;
  let before: string;
  try {
    before = await native.version();
  } catch {
    throw new AntigravityError('update-runtime-paused');
  }
  const unchanged = {
    previousVersion: before,
    version: before,
    restartedProcesses: 0,
    updateAttempted: false,
  };
  const failed = { ...unchanged, status: 'failed' as const, runtimePaused: true as const };
  try {
    return await registry.withLock(async () => {
      // A pending managed recovery record needs the runtime; never replace its native.
      if (registry.hasRecovery()) return failed;
      let attempted = false;
      try {
        await native.backup();
        // Recheck last: nothing is awaited between this and the native update.
        if (!(await deps.stillPaused())) return failed;
        attempted = true;
        await native.update();
        const after = await native.version();
        return {
          status: after === before ? ('current' as const) : ('updated' as const),
          previousVersion: before,
          version: after,
          restartedProcesses: 0,
          updateAttempted: true,
        };
      } catch {
        return { ...failed, updateAttempted: attempted };
      }
    });
  } catch {
    return failed;
  }
}

/**
 * Chooses the managed coordinator or the paused-runtime path. The paused path
 * runs only when nothing listens on the runtime socket AND the installed build
 * was read and matches no reviewed native (a read failure is not proof). A live (or unclear) runtime always keeps the
 * coordinator, and a reviewed build with a stopped runtime still fails there
 * (runtime unavailable) instead of updating without it.
 */
export async function routeManagedAntigravityUpdate(deps: {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  native: ManagedNativeUpdate;
  idleOnly?: boolean;
  /** Hashes the native once; only 'unreviewed' can take the paused path. */
  nativeReview: () => NativeReviewState;
  runtimeSocket: () => Promise<RuntimeSocketState>;
}): Promise<ManagedUpdateResult> {
  // The cheap socket probe runs before the hash, which is skipped for a live runtime.
  const paused = async () =>
    (await deps.runtimeSocket()) === 'stopped' && deps.nativeReview() === 'unreviewed';
  if (await paused())
    return updatePausedAntigravity({
      registry: deps.registry,
      native: deps.native,
      stillPaused: paused,
    });
  return updateManagedAntigravity({
    registry: deps.registry,
    driver: deps.driver,
    native: deps.native,
    idleOnly: deps.idleOnly,
  });
}
