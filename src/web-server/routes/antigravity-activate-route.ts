import { AntigravityError } from '../../antigravity/errors';
import type { ActivationResult } from '../../antigravity/types';
import type { AntigravityApiDependencies } from '../../antigravity/usage-contract';
import { email, record, safeId, timestamp } from '../../antigravity/usage-normalization';
import { createLogger } from '../../services/logging';
import type { Request, Response, Router } from 'express';
import { callerKey, ConfirmationBindings } from './caller-bound-confirmations';

const logger = createLogger('antigravity');

const ACTIVATION_STATUSES = [
  'active',
  'already-active',
  'busy',
  'confirmation-required',
  'stale-confirmation',
  'invalid-profile',
  'unsupported-runtime-probe',
  'deferred',
  'failed-rolled-back',
  'recovery-required',
];
const ACTIVATION_REASONS = [
  'activation-running',
  'running-processes',
  'unreviewed-processes',
  'active-identity-changed',
  'quota-changed',
  'identity-verification-failed',
  'transaction-failed',
  'foreign-replacement',
];

function integer(value: unknown, minimum: number, maximum: number): value is number {
  return (
    typeof value === 'number' && Number.isInteger(value) && value >= minimum && value <= maximum
  );
}

function confirmationToken(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{16,256}$/.test(value);
}

/** The public activation return is rebuilt so adapter-private fields cannot leak. */
function activationResponse(result: ActivationResult, profileId: string): Record<string, unknown> {
  if (
    result.hostId !== 'ubuntu' ||
    result.profileId !== profileId ||
    !ACTIVATION_STATUSES.includes(result.status)
  )
    throw new AntigravityError('Invalid activation result.');
  const output: Record<string, unknown> = {
    status: result.status,
    profileId,
    hostId: 'ubuntu',
    ...(email(result.email) ? { email: email(result.email) } : {}),
  };
  if (result.reason && ACTIVATION_REASONS.includes(result.reason)) output.reason = result.reason;
  const offer = result.confirmation;
  if (offer) {
    if (
      offer.profileId !== profileId ||
      offer.hostId !== 'ubuntu' ||
      !confirmationToken(offer.token) ||
      !timestamp(offer.expiresAt) ||
      !email(offer.email) ||
      !Array.isArray(offer.processes) ||
      offer.processes.length > 32
    )
      throw new AntigravityError('Invalid activation confirmation.');
    output.confirmation = {
      token: offer.token,
      expiresAt: timestamp(offer.expiresAt),
      profileId,
      hostId: 'ubuntu',
      email: email(offer.email),
      warning:
        'Another Antigravity program is running on Ubuntu. Stop the listed programs, switch accounts, and restore their reviewed sessions?',
      processes: offer.processes.map((process) => {
        const labels = {
          cli: 'Antigravity CLI',
          desktop: 'Antigravity Desktop',
          'language-server': 'Antigravity language server',
        };
        if (
          !integer(process.pid, 1, 2_147_483_647) ||
          !Object.prototype.hasOwnProperty.call(labels, process.role)
        )
          throw new AntigravityError('Invalid activation process.');
        return {
          pid: process.pid,
          role: process.role,
          label: labels[process.role],
        };
      }),
    };
  }
  return output;
}

/** POST /profiles/:profileId/activate and /confirm. Failures log status and reason, never secrets. */
export function registerAntigravityActivateRoutes(
  router: Router,
  deps: AntigravityApiDependencies,
  writeAllowed: (req: Request, res: Response) => boolean,
  confirmations: ConfirmationBindings
): void {
  const activate =
    (confirmOnly: boolean) =>
    async (req: Request, res: Response): Promise<void> => {
      if (!writeAllowed(req, res)) return;
      if (
        !safeId(req.params.profileId) ||
        Object.keys(req.query).length ||
        !record(req.body) ||
        Object.keys(req.body).some((key) => !['hostId', 'confirmationToken'].includes(key)) ||
        req.body.hostId !== 'ubuntu' ||
        (req.body.confirmationToken !== undefined &&
          !confirmationToken(req.body.confirmationToken)) ||
        (confirmOnly && !confirmationToken(req.body.confirmationToken))
      ) {
        res.status(400).json({
          error:
            'Select an Antigravity profile on Ubuntu and provide a valid confirmation when required.',
        });
        return;
      }
      if (
        typeof req.body.confirmationToken === 'string' &&
        !confirmations.allows(req.body.confirmationToken, callerKey(req))
      ) {
        // Issued to another device or browser: never consumed here.
        res.status(409).json({
          status: 'stale-confirmation',
          profileId: req.params.profileId,
          hostId: 'ubuntu',
        });
        return;
      }
      let invalidate = false;
      try {
        const result = await deps.activate({
          profileId: req.params.profileId,
          hostId: 'ubuntu',
          mode: 'manual',
          ...(typeof req.body.confirmationToken === 'string'
            ? { confirmationToken: req.body.confirmationToken }
            : {}),
        });
        invalidate = [
          'active',
          'already-active',
          'failed-rolled-back',
          'recovery-required',
        ].includes(result.status);
        const status =
          result.status === 'active' || result.status === 'already-active'
            ? 200
            : result.status === 'invalid-profile'
              ? 400
              : result.status === 'failed-rolled-back' || result.status === 'recovery-required'
                ? 500
                : 409;
        const body = activationResponse(result, req.params.profileId);
        const offer = body.confirmation as { token?: unknown } | undefined;
        if (offer && typeof offer.token === 'string')
          confirmations.record(offer.token, callerKey(req));
        if (status === 500)
          logger.warn('antigravity.activation.failed', 'Antigravity activation failed', {
            profileId: req.params.profileId,
            status: result.status,
            ...(result.reason ? { reason: result.reason } : {}),
          });
        res.status(status).json(body);
      } catch {
        invalidate = true;
        logger.error('antigravity.activation.error', 'Antigravity activation threw safely', {
          profileId: req.params.profileId,
        });
        res.status(500).json({
          error:
            'Antigravity account activation failed safely. Refresh the account list before retrying.',
        });
      } finally {
        if (invalidate) deps.invalidateUsage?.();
      }
    };
  router.post('/profiles/:profileId/activate', activate(false));
  router.post('/profiles/:profileId/confirm', activate(true));
}
