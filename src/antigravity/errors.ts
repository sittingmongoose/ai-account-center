import { CCSError } from '../errors/error-types';

/** Fixed provider-domain failures; callers must never put private adapter details here. */
export class AntigravityError extends CCSError {
  constructor(message: string) {
    super(message);
    this.name = 'AntigravityError';
  }
}
