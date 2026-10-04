import { AntigravityError } from './errors';
import { AntigravityConfirmationStore, checkedPlan, planFingerprint } from './confirmation';
import {
  AntigravityProfileRegistry,
  PrivateStorageError,
  assertUbuntu,
  checkedIdentity,
  credentialFingerprint,
  identityKey,
} from './registry';
import type {
  ActivateRequest,
  ActivationResult,
  AntigravitySwitchDriver,
  InstallReceipt,
  NativeCredential,
  ProcessPlan,
  ProfileDto,
  StopReceipt,
  VerifiedIdentity,
  QuiescedHostProof,
} from './types';

const IDENTITY_MAX_AGE = 5 * 60_000;
/** The exact broker-absence sentinel the bridge and driver raise. */
const RUNTIME_UNAVAILABLE = 'antigravity-runtime-unavailable';

export type AntigravityRecoveryStatus =
  | 'completed'
  | 'aborted'
  | 'restored-previous'
  | 'recovery-required'
  | 'no-recovery-pending';

export interface AntigravityRecoveryResult {
  status: AntigravityRecoveryStatus;
  hostId: 'ubuntu';
  profileId?: string;
  email?: string;
}

interface ServiceOptions {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  now?: () => number;
  confirmations?: AntigravityConfirmationStore;
}

/** No error message/tool output from a driver is forwarded to callers. */
export class AntigravitySwitchService {
  private readonly registry: AntigravityProfileRegistry;
  private readonly driver: AntigravitySwitchDriver;
  private readonly now: () => number;
  private readonly confirmations: AntigravityConfirmationStore;

  constructor(options: ServiceOptions) {
    assertUbuntu(options.driver.hostId);
    this.registry = options.registry;
    this.driver = options.driver;
    this.now = options.now ?? Date.now;
    this.confirmations = options.confirmations ?? new AntigravityConfirmationStore();
  }

  listProfiles(): ProfileDto[] {
    return this.registry.listProfiles();
  }

  /** Selected native credential and proven running account are deliberately distinct. */
  async readInventory(): Promise<ProfileDto[]> {
    const profiles = this.listProfiles();
    try {
      const live = await this.driver.readCurrentCredential();
      const identity = await this.verified(live);
      const selectedKey = identityKey(identity);
      return profiles.map((profile) => ({
        ...profile,
        hosts: profile.hosts.map((host) => ({
          ...host,
          selected: profile.identityKey === selectedKey,
          // A stored runtime proof never labels a different current native account active.
          active: host.active && profile.identityKey === selectedKey,
        })),
      }));
    } catch {
      return profiles.map((profile) => ({
        ...profile,
        hosts: profile.hosts.map((host) => ({
          ...host,
          selected: false,
          active: false,
        })),
      }));
    }
  }

  /** Import is explicit, identity-deduplicated and never changes the live native login. */
  async importNativeProfile(profileId: string, credential: NativeCredential): Promise<ProfileDto> {
    return this.registry.withLock(async () => {
      if (this.registry.hasRecovery()) throw new PrivateStorageError('recovery-required');
      const identity = await this.verified(credential);
      this.registry.saveCredential(profileId, 'ubuntu', credential, identity, this.now());
      const profile = this.listProfiles().find((entry) => entry.id === profileId);
      if (!profile) throw new PrivateStorageError('corrupt');
      return profile;
    });
  }

  private async verified(credential: NativeCredential): Promise<VerifiedIdentity> {
    const identity = checkedIdentity(await this.driver.validateCredential(credential));
    const age = this.now() - Date.parse(identity.verifiedAt);
    if (age > IDENTITY_MAX_AGE || age < -60_000)
      throw new AntigravityError('Identity proof is stale.');
    return identity;
  }

  /**
   * The broker's owned-idle approval, probed before anything is offered or
   * stopped. A probe that fails (for example a runtime proof that is not
   * ready yet) counts as a refusal, so the caller answers busy.
   */
  private async ownedIdleApproved(
    plan: ProcessPlan,
    current: VerifiedIdentity,
    credential: NativeCredential
  ): Promise<boolean> {
    if (!this.driver.approveOwnedIdlePlan) return false;
    try {
      return await this.driver.approveOwnedIdlePlan(
        plan,
        current,
        credentialFingerprint(credential)
      );
    } catch {
      return false;
    }
  }

  async activate(request: ActivateRequest): Promise<ActivationResult> {
    const result = (
      status: ActivationResult['status'],
      extra: Partial<ActivationResult> = {}
    ): ActivationResult => ({
      status,
      profileId: request.profileId,
      hostId: 'ubuntu',
      ...extra,
    });
    try {
      assertUbuntu(request.hostId);
      if (
        !['manual', 'automatic'].includes(request.mode) ||
        (request.confirmationToken !== undefined &&
          (typeof request.confirmationToken !== 'string' ||
            !/^[A-Za-z0-9_-]{43}$/.test(request.confirmationToken))) ||
        (request.mode === 'automatic' && request.confirmationToken !== undefined)
      ) {
        return result('invalid-profile');
      }
      return await this.registry.withLock(async () => {
        if (this.registry.hasRecovery()) return result('recovery-required');
        const targetRecord = this.registry.readCredential(request.profileId, 'ubuntu');
        const target = await this.verified(targetRecord.credential);
        if (identityKey(target) !== identityKey(targetRecord.identity))
          return result('invalid-profile');
        const currentCredential = await this.driver.readCurrentCredential();
        const current = await this.verified(currentCredential);
        const previousId = this.registry.findIdentity(current);
        if (!previousId) return result('invalid-profile');
        const currentKey = identityKey(current);
        const targetKey = identityKey(target);
        const publicIdentity = { email: target.email };
        // Reject unsupported production probes before metadata/native writes or stopping anything.
        if (!(await this.driver.canProveRuntimeIdentity()))
          return result('unsupported-runtime-probe', publicIdentity);
        const initialPlan = checkedPlan(await this.driver.inspectProcesses());
        if (!initialPlan.complete)
          return result('busy', {
            ...publicIdentity,
            reason: 'unreviewed-processes',
          });
        if (request.mode === 'automatic') {
          if (
            initialPlan.processes.length &&
            (!initialPlan.continuity?.restorable ||
              !this.driver.approveOwnedIdlePlan ||
              !(await this.ownedIdleApproved(initialPlan, current, currentCredential)))
          )
            return result('busy', {
              ...publicIdentity,
              reason: 'running-processes',
            });
          if (
            !request.expectedActiveIdentityKey ||
            request.expectedActiveIdentityKey !== currentKey
          ) {
            return result('deferred', {
              ...publicIdentity,
              reason: 'active-identity-changed',
            });
          }
          if (!request.revalidateAutomatic)
            return result('deferred', {
              ...publicIdentity,
              reason: 'quota-changed',
            });
        }
        const revalidateAuto = async (quiescedHost?: QuiescedHostProof): Promise<boolean> => {
          if (request.mode !== 'automatic') return true;
          const revalidate = request.revalidateAutomatic;
          if (!revalidate) return false;
          try {
            return await revalidate({
              hostId: 'ubuntu',
              currentIdentityKey: currentKey,
              targetIdentityKey: targetKey,
              currentEmail: current.email,
              targetEmail: target.email,
              phase: quiescedHost ? 'before-install' : 'before-stop',
              ...(quiescedHost ? { quiescedHost } : {}),
            });
          } catch {
            return false;
          }
        };
        if (!(await revalidateAuto()))
          return result('deferred', {
            ...publicIdentity,
            reason: 'quota-changed',
          });
        const binding = {
          profileId: request.profileId,
          currentIdentityKey: currentKey,
          targetIdentityKey: targetKey,
          credentialFingerprint: credentialFingerprint(targetRecord.credential),
          planFingerprint: planFingerprint(initialPlan),
        };
        if (request.confirmationToken) {
          if (!this.confirmations.consume(request.confirmationToken, binding, this.now())) {
            return result('stale-confirmation', publicIdentity);
          }
        } else if (
          request.mode === 'manual' &&
          initialPlan.processes.length &&
          currentKey !== targetKey
        ) {
          if (!initialPlan.continuity?.restorable) {
            return result('busy', {
              ...publicIdentity,
              reason: 'unreviewed-processes',
            });
          }
          // Never offer a stop-and-switch the broker cannot honour: the same
          // owned-idle approval the confirmed switch requires (switchUnderLock)
          // is probed first, and its refusal keeps the existing busy result.
          if (
            this.driver.approveOwnedIdlePlan &&
            !(await this.ownedIdleApproved(initialPlan, current, currentCredential))
          ) {
            return result('busy', {
              ...publicIdentity,
              reason: 'running-processes',
            });
          }
          return result('confirmation-required', {
            ...publicIdentity,
            confirmation: this.confirmations.issue(binding, target.email, initialPlan, this.now()),
          });
        }
        if (currentKey === targetKey) {
          // No stop ever happens on this path, so the broker holds no
          // transaction state to complete.
          const proof = await this.proveLive(target, null, false);
          const live = await this.driver.readCurrentCredential();
          const liveIdentity = await this.verified(live);
          if (
            proof.fingerprint !== credentialFingerprint(live) ||
            identityKey(liveIdentity) !== targetKey
          )
            return result('invalid-profile');
          this.registry.saveCredential(previousId, 'ubuntu', live, liveIdentity, this.now());
          this.registry.completeTransaction(previousId, proof.verifiedAt);
          return result('already-active', publicIdentity);
        }
        return await this.switchUnderLock(
          request,
          targetRecord.credential,
          target,
          currentCredential,
          current,
          previousId,
          initialPlan,
          revalidateAuto,
          result
        );
      });
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy') {
        return result('busy', { reason: 'activation-running' });
      }
      if (error instanceof PrivateStorageError && error.code === 'recovery-required') {
        return result('recovery-required');
      }
      return result('invalid-profile', {
        reason: 'identity-verification-failed',
      });
    }
  }

  /**
   * Finish or undo a stuck switch. The live login is re-proved through the
   * same live verification the switcher uses, then the transaction either
   * completes (live is the target) or aborts (live is the previous, or the
   * previous saved revision is restored and re-proved). Anything unproven
   * leaves recovery-required in place. Saved profiles are never rewritten.
   */
  async recover(): Promise<AntigravityRecoveryResult> {
    const result = (
      status: AntigravityRecoveryStatus,
      extra: Partial<AntigravityRecoveryResult> = {}
    ): AntigravityRecoveryResult => ({ status, hostId: 'ubuntu', ...extra });
    try {
      return await this.registry.withLock(async () => {
        const tx = this.registry.pendingTransaction();
        if (!tx) return result('no-recovery-pending');
        // Hygiene first: no owned writer may move the live login under
        // the proof. Any failure here keeps the transaction stuck.
        await this.driver.stopOwnedRestarts();
        const live = await this.driver.readCurrentCredential();
        const proven = await this.verified(live);
        const liveKey = identityKey(proven);
        const target = this.registry.readCredential(tx.targetId, 'ubuntu');
        const previous = this.registry.readCredential(tx.previousId, 'ubuntu');
        if (liveKey === identityKey(target.identity)) {
          this.registry.completeTransaction(tx.targetId, proven.verifiedAt);
          return result('completed', {
            profileId: tx.targetId,
            email: target.identity.email,
          });
        }
        if (liveKey === identityKey(previous.identity)) {
          this.registry.abortTransaction();
          return result('aborted', {
            profileId: tx.previousId,
            email: previous.identity.email,
          });
        }
        // Unknown live login: restore the previous saved revision only
        // when it still verifies, then re-prove before clearing anything.
        const previousIdentity = await this.verified(previous.credential);
        if (identityKey(previousIdentity) !== identityKey(previous.identity))
          return result('recovery-required');
        const receipt = await this.driver.installCredential(
          previous.credential,
          credentialFingerprint(live)
        );
        if (receipt.installedFingerprint !== credentialFingerprint(previous.credential))
          return result('recovery-required');
        const restored = await this.driver.readCurrentCredential();
        const reproven = await this.verified(restored);
        if (identityKey(reproven) !== identityKey(previous.identity))
          return result('recovery-required');
        this.registry.abortTransaction();
        return result('restored-previous', {
          profileId: tx.previousId,
          email: previous.identity.email,
        });
      });
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy') throw error;
      return result('recovery-required');
    }
  }

  private checkRuntimeProof(
    proof: Awaited<ReturnType<AntigravitySwitchDriver['proveRuntimeIdentity']>>,
    expected: VerifiedIdentity,
    requireSession: boolean
  ): void {
    const identity = checkedIdentity(proof.identity);
    const age = this.now() - Date.parse(identity.verifiedAt);
    if (
      !proof.runtimeStarted ||
      identity.source !== 'native-runtime' ||
      identityKey(identity) !== identityKey(expected) ||
      age > IDENTITY_MAX_AGE ||
      age < -60_000 ||
      !/^[a-f0-9]{64}$/.test(proof.credentialFingerprint) ||
      (requireSession && !proof.sessionRestored)
    )
      throw new AntigravityError('Runtime verification failed.');
  }

  /**
   * A broker session proof needs a stopped-and-restarted session, which the
   * idle path deliberately never creates. The live broker then raises
   * antigravity-runtime-unavailable; only that absence falls back to the
   * live-verified native proof (fresh Google userinfo bound to the installed
   * bytes). A returned proof is always checked strictly and never falls back.
   */
  private async proveLive(
    expected: VerifiedIdentity,
    expectedFingerprint: string | null,
    sessionExpected: boolean
  ): Promise<{ verifiedAt: string; fingerprint: string }> {
    if (!sessionExpected) {
      try {
        const proof = await this.driver.proveRuntimeIdentity(expected);
        this.checkRuntimeProof(proof, expected, false);
        return {
          verifiedAt: proof.identity.verifiedAt,
          fingerprint: proof.credentialFingerprint,
        };
      } catch (error) {
        if (!(error instanceof AntigravityError) || error.message !== RUNTIME_UNAVAILABLE)
          throw error;
      }
      const live = await this.driver.readCurrentCredential();
      const identity = await this.verified(live);
      if (
        identityKey(identity) !== identityKey(expected) ||
        (expectedFingerprint !== null && credentialFingerprint(live) !== expectedFingerprint)
      )
        throw new AntigravityError('Idle identity proof failed.');
      return { verifiedAt: identity.verifiedAt, fingerprint: credentialFingerprint(live) };
    }
    const proof = await this.driver.proveRuntimeIdentity(expected);
    this.checkRuntimeProof(proof, expected, true);
    return { verifiedAt: proof.identity.verifiedAt, fingerprint: proof.credentialFingerprint };
  }

  private async switchUnderLock(
    request: ActivateRequest,
    targetCredential: NativeCredential,
    target: VerifiedIdentity,
    previousCredential: NativeCredential,
    previous: VerifiedIdentity,
    previousId: string,
    initialPlan: ProcessPlan,
    revalidateAuto: (quiescedHost?: QuiescedHostProof) => Promise<boolean>,
    result: (
      status: ActivationResult['status'],
      extra?: Partial<ActivationResult>
    ) => ActivationResult
  ): Promise<ActivationResult> {
    let stopReceipt: StopReceipt | undefined;
    let installReceipt: InstallReceipt | undefined;
    let stopAttempted = false;
    let intentRecorded = false;
    let durableCommitted = false;
    let original = previousCredential;
    try {
      // A complete final census is checked even after a valid confirmation.
      const beforeStop = checkedPlan(await this.driver.inspectProcesses());
      if (planFingerprint(beforeStop) !== planFingerprint(initialPlan)) {
        return result(request.mode === 'automatic' ? 'busy' : 'stale-confirmation', {
          email: target.email,
          reason: 'running-processes',
        });
      }
      if (!(await revalidateAuto()))
        return result('deferred', {
          email: target.email,
          reason: 'quota-changed',
        });
      if (
        (request.mode === 'automatic' || this.driver.approveOwnedIdlePlan) &&
        beforeStop.processes.length &&
        (!this.driver.approveOwnedIdlePlan ||
          !(await this.driver.approveOwnedIdlePlan(
            beforeStop,
            previous,
            credentialFingerprint(original)
          )))
      )
        return result(request.mode === 'automatic' ? 'busy' : 'stale-confirmation', {
          email: target.email,
          reason: 'running-processes',
        });
      this.registry.saveCredential(previousId, 'ubuntu', original, previous, this.now());
      this.registry.beginTransaction(request.profileId, previousId, this.now());
      intentRecorded = true;
      if (beforeStop.processes.length) {
        if (
          (request.mode === 'manual' && !request.confirmationToken) ||
          (request.mode === 'automatic' && !this.driver.approveOwnedIdlePlan) ||
          !beforeStop.continuity?.restorable
        ) {
          throw new AntigravityError('Unapproved stop.');
        }
        stopAttempted = true;
        stopReceipt = await this.driver.stopProcesses(beforeStop);
        const stopped = stopReceipt.stopped.map((identity) => JSON.stringify(identity)).sort();
        const approved = beforeStop.processes
          .map((process) => JSON.stringify(process.identity))
          .sort();
        if (!stopReceipt.complete || JSON.stringify(stopped) !== JSON.stringify(approved)) {
          throw new AntigravityError('Incomplete reviewed stop.');
        }
      }
      // Shutdown may refresh native auth. Preserve the freshest original bytes.
      original = await this.driver.readCurrentCredential();
      const shutdownIdentity = await this.verified(original);
      if (identityKey(shutdownIdentity) !== identityKey(previous))
        throw new AntigravityError('Live account changed.');
      this.registry.saveCredential(previousId, 'ubuntu', original, shutdownIdentity, this.now());
      const beforeInstall = checkedPlan(await this.driver.inspectProcesses());
      if (!beforeInstall.complete || beforeInstall.processes.length)
        throw new AntigravityError('A new writer appeared.');
      let quiescedHost: QuiescedHostProof | undefined;
      if (stopReceipt && this.driver.revalidateQuiescedStop) {
        quiescedHost =
          (await this.driver.revalidateQuiescedStop(
            stopReceipt,
            shutdownIdentity,
            credentialFingerprint(original)
          )) ?? undefined;
      }
      if (request.mode === 'automatic' && stopReceipt && !quiescedHost)
        throw new AntigravityError('Owned stop proof unavailable.');
      if (!(await revalidateAuto(quiescedHost))) {
        if (request.mode === 'automatic') {
          if (stopReceipt) {
            await this.driver.restartProcesses(stopReceipt);
            const restored = await this.driver.proveRuntimeIdentity(previous);
            this.checkRuntimeProof(restored, previous, true);
            const resumed = await this.driver.readCurrentCredential();
            if (
              credentialFingerprint(resumed) !== restored.credentialFingerprint ||
              identityKey(await this.verified(resumed)) !== identityKey(previous)
            )
              throw new AntigravityError('Prior runtime changed.');
          }
          this.registry.abortTransaction();
          durableCommitted = true;
          if (stopReceipt && this.driver.completeActivation)
            await this.driver.completeActivation(previous);
          return result('deferred', {
            email: target.email,
            reason: 'quota-changed',
          });
        }
        throw new AntigravityError('Policy changed.');
      }
      // No await intervenes between the last automatic policy result and starting the native write.
      installReceipt = await this.driver.installCredential(
        targetCredential,
        credentialFingerprint(original)
      );
      if (installReceipt.installedFingerprint !== credentialFingerprint(targetCredential)) {
        throw new AntigravityError('Installed fingerprint mismatch.');
      }
      const storedIdentity = checkedIdentity(await this.driver.readStoredIdentity());
      if (identityKey(storedIdentity) !== identityKey(target))
        throw new AntigravityError('Stored identity mismatch.');
      if (stopReceipt) await this.driver.restartProcesses(stopReceipt);
      // The broker only holds transaction state to complete when it
      // stopped and restarted a session on this path.
      const sessionExpected = !!stopReceipt?.stopped.length;
      const proof = await this.proveLive(
        target,
        credentialFingerprint(targetCredential),
        sessionExpected
      );
      const refreshed = await this.driver.readCurrentCredential();
      const refreshedIdentity = await this.verified(refreshed);
      if (
        credentialFingerprint(refreshed) !== proof.fingerprint ||
        identityKey(refreshedIdentity) !== identityKey(target)
      )
        throw new AntigravityError('Runtime credential changed.');
      this.registry.saveCredential(
        request.profileId,
        'ubuntu',
        refreshed,
        refreshedIdentity,
        this.now()
      );
      this.registry.completeTransaction(request.profileId, proof.verifiedAt);
      durableCommitted = true;
      if (sessionExpected && this.driver.completeActivation)
        await this.driver.completeActivation(target);
      return result('active', { email: target.email });
    } catch {
      if (!intentRecorded) return result('invalid-profile', { reason: 'transaction-failed' });
      // A commit acknowledgement may have been lost after the foreground was
      // released. Never stop that session or roll its account back afterwards.
      if (durableCommitted) {
        try {
          this.registry.beginTransaction(request.profileId, previousId, this.now());
          this.registry.markRecovery();
        } catch {
          /* Keep the driver-held blocked/uncertain transaction. */
        }
        return result('recovery-required', { reason: 'transaction-failed' });
      }
      try {
        await this.driver.stopOwnedRestarts();
        if (stopAttempted && !stopReceipt) throw new AntigravityError('Unknown partial stop.');
        if (installReceipt && !(await this.driver.rollbackCredential(installReceipt, original))) {
          this.registry.markRecovery();
          return result('recovery-required', { reason: 'foreign-replacement' });
        }
        const rollbackIdentity = await this.verified(await this.driver.readCurrentCredential());
        if (identityKey(rollbackIdentity) !== identityKey(previous))
          throw new AntigravityError('Rollback identity mismatch.');
        if (stopReceipt) await this.driver.restartProcesses(stopReceipt);
        const rollbackSessionExpected = !!stopReceipt?.stopped.length;
        const proof = await this.proveLive(
          previous,
          credentialFingerprint(original),
          rollbackSessionExpected
        );
        const recovered = await this.driver.readCurrentCredential();
        if (
          credentialFingerprint(recovered) !== proof.fingerprint ||
          identityKey(await this.verified(recovered)) !== identityKey(previous)
        )
          throw new AntigravityError('Recovered native account changed.');
        this.registry.completeTransaction(previousId, proof.verifiedAt);
        durableCommitted = true;
        if (rollbackSessionExpected && this.driver.completeActivation)
          await this.driver.completeActivation(previous);
        return result('failed-rolled-back', { reason: 'transaction-failed' });
      } catch {
        try {
          if (durableCommitted)
            this.registry.beginTransaction(request.profileId, previousId, this.now());
          this.registry.markRecovery();
        } catch {
          /* Immutable intent already blocks another switch. */
        }
        return result('recovery-required', { reason: 'transaction-failed' });
      }
    }
  }
}
