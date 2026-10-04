import { AntigravityError } from '../../antigravity/errors';
import { PrivateStorageError } from '../../antigravity/registry';
import type { AntigravityRecoveryResult } from '../../antigravity/switch-service';
import type { AntigravityApiDependencies } from '../../antigravity/usage-contract';
import { email, record, safeId } from '../../antigravity/usage-normalization';
import { createLogger } from '../../services/logging';
import type { Request, Response, Router } from 'express';

const logger = createLogger('antigravity');

const RECOVERY_STATUSES = [
  'completed',
  'aborted',
  'restored-previous',
  'recovery-required',
  'no-recovery-pending',
];

/** The public recovery return is rebuilt so adapter-private fields cannot leak. */
function recoveryResponse(result: AntigravityRecoveryResult): Record<string, unknown> {
  if (result.hostId !== 'ubuntu' || !RECOVERY_STATUSES.includes(result.status))
    throw new AntigravityError('Invalid Antigravity recovery result.');
  if (
    (result.profileId !== undefined && !safeId(result.profileId)) ||
    (result.email !== undefined && !email(result.email))
  )
    throw new AntigravityError('Invalid Antigravity recovery result.');
  return {
    status: result.status,
    hostId: 'ubuntu',
    ...(result.profileId ? { profileId: result.profileId } : {}),
    ...(result.email ? { email: result.email } : {}),
  };
}

/**
 * POST /recover: finish or undo a stuck Ubuntu switch. Guarded exactly like
 * activation (dashboard session, origin, JSON). A running switch answers 409
 * instead of being disturbed; anything still unproven answers 500 with the
 * transaction untouched.
 */
export function registerAntigravityRecoverRoute(
  router: Router,
  deps: AntigravityApiDependencies,
  writeAllowed: (req: Request, res: Response) => boolean
): void {
  router.post('/recover', async (req: Request, res: Response): Promise<void> => {
    if (!writeAllowed(req, res)) return;
    if (
      Object.keys(req.query).length ||
      !record(req.body) ||
      Object.keys(req.body).length !== 1 ||
      req.body.hostId !== 'ubuntu'
    ) {
      res.status(400).json({
        error: 'Recover an Antigravity switch on Ubuntu only.',
      });
      return;
    }
    try {
      const result = await deps.recover();
      if (['completed', 'aborted', 'restored-previous'].includes(result.status))
        deps.invalidateUsage?.();
      if (result.status === 'recovery-required')
        logger.warn('antigravity.recovery.stuck', 'Antigravity recovery left the switch stuck', {
          ...(result.profileId ? { profileId: result.profileId } : {}),
        });
      else
        logger.info('antigravity.recovery.done', 'Antigravity recovery finished', {
          status: result.status,
          ...(result.profileId ? { profileId: result.profileId } : {}),
        });
      res.status(result.status === 'recovery-required' ? 500 : 200).json(recoveryResponse(result));
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy') {
        res.status(409).json({
          error: 'An Antigravity switch is running. Try recovery again when it finishes.',
        });
        return;
      }
      logger.error('antigravity.recovery.error', 'Antigravity recovery threw safely', {});
      res.status(500).json({
        error:
          'Antigravity recovery failed safely. The stuck switch is unchanged; try again later.',
      });
    }
  });
}
