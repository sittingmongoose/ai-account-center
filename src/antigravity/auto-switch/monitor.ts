import {
  copySettings,
  defaultAntigravityAutoSwitchState,
  mergeAntigravityAutoSwitchSettings,
  validateAntigravityAutoSwitchStoredState,
} from './settings';
import { decideAntigravityAutoSwitch, sameAntigravityDecision } from './policy';
import type {
  AntigravityAutoSwitchDeps,
  AntigravityAutoSwitchOutcome,
  AntigravityAutoSwitchSettingsPatch,
  AntigravityAutoSwitchStatus,
  AntigravityAutoSwitchStoredState,
} from './types';

export const ANTIGRAVITY_AUTO_SWITCH_MESSAGES: Record<AntigravityAutoSwitchOutcome, string> = {
  disabled: 'Antigravity automatic switching is off.',
  scheduled: 'Checking the selected Antigravity model pool.',
  setup_required: 'Add two verified Ubuntu accounts and choose their model quota pool.',
  healthy: 'The active Antigravity account has available quota in the selected pool.',
  no_fresh_quota: 'Waiting for fresh account-bound Antigravity quota and its reset time.',
  no_candidate: 'No other verified account has enough fresh quota in the selected pool.',
  waiting_idle: 'Waiting for the Ubuntu Antigravity CLI and account activation to become idle.',
  cooldown: 'Waiting after the last Antigravity account switch.',
  switching: 'Switching the Ubuntu Antigravity account.',
  switched: 'The Ubuntu Antigravity account was switched.',
  deferred: 'The account, settings or quota changed. A later check will retry.',
  error: 'Antigravity automatic switching could not complete its check.',
};

const ERROR_BACKOFF_MS = 300_000;

function sameStateSettings(
  a: AntigravityAutoSwitchStoredState,
  b: AntigravityAutoSwitchStoredState
): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Independent monitor: no Codex configuration, CLI prompts or forced process stops. */
export class AntigravityAutoSwitchService {
  private timer: unknown;
  private running = false;
  private stopped = false;
  private generation = 0;
  private inFlight: Promise<void> | null = null;
  private activationInProgress = false;
  private outcome: AntigravityAutoSwitchOutcome = 'disabled';
  private lastCheckedAt?: string;
  private retryAfter = 0;
  private unpersistedLastSwitch: AntigravityAutoSwitchStoredState['lastSwitch'] = null;

  constructor(private readonly deps: AntigravityAutoSwitchDeps) {}

  private readState(): AntigravityAutoSwitchStoredState {
    const state = validateAntigravityAutoSwitchStoredState(this.deps.store.read());
    if (
      this.unpersistedLastSwitch &&
      (!state.lastSwitch ||
        Date.parse(state.lastSwitch.at) < Date.parse(this.unpersistedLastSwitch.at))
    ) {
      state.lastSwitch = { ...this.unpersistedLastSwitch };
    }
    return state;
  }

  getStatus(): AntigravityAutoSwitchStatus {
    let state = defaultAntigravityAutoSwitchState();
    let outcome = this.outcome;
    try {
      state = this.readState();
      if (!state.settings.enabled && !this.activationInProgress) outcome = 'disabled';
      else if (state.settings.enabled && outcome === 'disabled') outcome = 'scheduled';
    } catch {
      outcome = 'error';
    }
    return {
      ...copySettings(state.settings),
      outcome,
      message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES[outcome],
      activationInProgress: this.activationInProgress,
      ...(this.lastCheckedAt ? { lastCheckedAt: this.lastCheckedAt } : {}),
      ...(state.lastSwitch
        ? {
            lastSwitchedAt: state.lastSwitch.at,
            lastProfileId: state.lastSwitch.profileId,
            lastHostId: state.lastSwitch.hostId,
          }
        : {}),
    };
  }

  updateSettings(patch: AntigravityAutoSwitchSettingsPatch): AntigravityAutoSwitchStatus {
    const next = mergeAntigravityAutoSwitchSettings(this.readState(), patch);
    this.deps.store.write(next);
    this.generation++;
    this.retryAfter = 0;
    if (!this.activationInProgress) this.outcome = next.settings.enabled ? 'scheduled' : 'disabled';
    if (this.running)
      this.schedule(next.settings.enabled ? 1000 : next.settings.pollIntervalSeconds * 1000);
    return this.getStatus();
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.stopped = false;
    this.schedule(1000);
  }

  stop(): void {
    this.running = false;
    this.stopped = true;
    this.generation++;
    this.clearTimer();
  }

  private clearTimer(): void {
    if (this.timer === undefined) return;
    if (this.deps.clearTimer) this.deps.clearTimer(this.timer);
    else clearTimeout(this.timer as ReturnType<typeof setTimeout>);
    this.timer = undefined;
  }

  private schedule(delay: number): void {
    this.clearTimer();
    const callback = () => {
      this.timer = undefined;
      void this.runCycle().finally(() => {
        if (!this.running) return;
        let interval = 60_000;
        try {
          interval = this.readState().settings.pollIntervalSeconds * 1000;
        } catch {
          /* fail closed on cycle */
        }
        this.schedule(interval);
      });
    };
    if (this.deps.setTimer) this.timer = this.deps.setTimer(callback, delay);
    else {
      const timer = setTimeout(callback, delay);
      timer.unref();
      this.timer = timer;
    }
  }

  runCycle(): Promise<void> {
    if (this.inFlight) return this.inFlight;
    this.inFlight = Promise.resolve()
      .then(() => this.check())
      .finally(() => {
        this.inFlight = null;
      });
    return this.inFlight;
  }

  private async check(): Promise<void> {
    const now = this.deps.now ?? Date.now;
    const generation = this.generation;
    try {
      const state = this.readState();
      if (this.stopped || !state.settings.enabled) {
        this.outcome = 'disabled';
        return;
      }
      if (now() < this.retryAfter) return;
      this.lastCheckedAt = new Date(now()).toISOString();
      // Missing pool setup does not query vendor quota or native process state.
      if (!state.settings.requestedPoolId) {
        this.outcome = 'setup_required';
        return;
      }
      const observation = await this.deps.observe();
      if (
        this.stopped ||
        generation !== this.generation ||
        !sameStateSettings(state, this.readState())
      ) {
        this.outcome = this.readState().settings.enabled && !this.stopped ? 'deferred' : 'disabled';
        return;
      }
      const decision = decideAntigravityAutoSwitch(state, observation, now());
      this.outcome = decision.outcome;
      if (!decision.active || !decision.target || decision.outcome !== 'scheduled') return;
      const current = decision.active;
      const target = decision.target;
      this.activationInProgress = true;
      this.outcome = 'switching';
      let lastUnderLockValidationPassed = false;
      try {
        const result = await this.deps.activate({
          profileId: target.id,
          hostId: 'ubuntu',
          mode: 'automatic',
          expectedActiveIdentityKey: current.identityKey,
          revalidateAutomatic: async (context) => {
            // A later refused callback supersedes an earlier successful one.
            lastUnderLockValidationPassed = false;
            if (
              context.hostId !== 'ubuntu' ||
              context.currentIdentityKey !== current.identityKey ||
              context.targetIdentityKey !== target.identityKey ||
              this.stopped ||
              generation !== this.generation ||
              !sameStateSettings(state, this.readState())
            )
              return false;
            // Only the trusted transaction service can supply an earned stop
            // proof. It is not a cached native idle sample or a client setting.
            const latest = await this.deps.observe(
              context.phase === 'before-install' ? context.quiescedHost : undefined
            );
            if (
              this.stopped ||
              generation !== this.generation ||
              !sameStateSettings(state, this.readState())
            )
              return false;
            const valid = sameAntigravityDecision(
              decision,
              decideAntigravityAutoSwitch(state, latest, now())
            );
            lastUnderLockValidationPassed = valid;
            return valid;
          },
        });
        if (result.status === 'active' && lastUnderLockValidationPassed) {
          const lastSwitch = {
            at: new Date(now()).toISOString(),
            hostId: 'ubuntu' as const,
            profileId: target.id,
          };
          // Preserve any user changes made during the awaited native activation.
          const latest = this.readState();
          this.unpersistedLastSwitch = lastSwitch;
          this.deps.store.write({ ...latest, lastSwitch });
          this.unpersistedLastSwitch = null;
          this.outcome = 'switched';
        } else if (result.status === 'busy' || result.status === 'confirmation-required') {
          // The automatic path has no confirmation token and cannot approve a stop.
          this.outcome = 'waiting_idle';
        } else if (
          result.status === 'deferred' ||
          result.status === 'already-active' ||
          result.status === 'stale-confirmation'
        ) {
          this.outcome = 'deferred';
        } else {
          this.outcome = 'error';
          this.retryAfter = now() + ERROR_BACKOFF_MS;
        }
      } finally {
        this.activationInProgress = false;
      }
    } catch {
      // Raw API/driver errors may contain secrets. Status uses fixed public copy only.
      this.outcome = 'error';
      this.retryAfter = now() + ERROR_BACKOFF_MS;
    }
  }
}
