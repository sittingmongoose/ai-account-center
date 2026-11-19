import { lifecyclePlan } from '../../shared/private-file-lease';
import { LifecycleHttpError } from './account-lifecycle-accounts';
import {
  MAX_REGISTRY_ACCOUNTS,
  parseAccountRegistry,
  type AccountRegistryV2,
  type RegistryAccount,
} from './account-registry-v2';

export type RegistryEdit =
  | { kind: 'label'; id: string; label: string | null }
  | { kind: 'insert'; entry: RegistryAccount; uniqueProvider: boolean }
  | { kind: 'remove'; id: string }
  | { kind: 'restore'; entry: RegistryAccount; index: number };

type Transition =
  | { registry: AccountRegistryV2; position: number | null }
  | { refusal: 'missing' | 'single_account_provider' | 'too_many_accounts' };

/** Called by registry mutators while they hold the registered write lease. */
export function editRegistry(
  root: string,
  current: AccountRegistryV2,
  edit: RegistryEdit
): Transition {
  const reply = lifecyclePlan(root, {
    operation: 'registry-edit',
    registry: current,
    edit: edit.kind === 'insert' ? { ...edit, limit: MAX_REGISTRY_ACCOUNTS } : edit,
  });
  if (reply !== undefined) {
    const result = reply as Record<string, unknown> | null;
    if (
      result?.refusal === 'missing' ||
      result?.refusal === 'single_account_provider' ||
      result?.refusal === 'too_many_accounts'
    )
      return { refusal: result.refusal };
    const registry = parseAccountRegistry(result?.registry);
    const position = result?.position;
    if (
      !registry ||
      !(position === null || (typeof position === 'number' && Number.isSafeInteger(position)))
    )
      throw new LifecycleHttpError(500, 'write_failed');
    return { registry, position };
  }

  const accounts = [...current.accounts];
  let position: number | null = null;
  switch (edit.kind) {
    case 'label': {
      const index = accounts.findIndex((entry) => entry.id === edit.id);
      if (index < 0) return { refusal: 'missing' };
      accounts[index] = { ...accounts[index], label: edit.label };
      break;
    }
    case 'insert':
      if (edit.uniqueProvider && accounts.some((entry) => entry.provider === edit.entry.provider))
        return { refusal: 'single_account_provider' };
      if (accounts.length >= MAX_REGISTRY_ACCOUNTS) return { refusal: 'too_many_accounts' };
      accounts.push(edit.entry);
      break;
    case 'remove': {
      const index = accounts.findIndex((entry) => entry.id === edit.id);
      if (index < 0) return { refusal: 'missing' };
      accounts.splice(index, 1);
      position = index;
      break;
    }
    case 'restore':
      if (!accounts.some((entry) => entry.id === edit.entry.id))
        accounts.splice(Math.min(edit.index, accounts.length), 0, edit.entry);
      break;
  }
  return { registry: { ...current, accounts }, position };
}

export function keyProbeOutcome(root: string, status: string): 'rejected' | 'ok' | 'unverified' {
  const reply = lifecyclePlan(root, { operation: 'key-probe', status });
  if (reply === undefined) {
    if (status === 'needs_sign_in') return 'rejected';
    return status === 'ok' || status === 'cached' ? 'ok' : 'unverified';
  }
  if (reply === 'rejected' || reply === 'ok' || reply === 'unverified') return reply;
  throw new LifecycleHttpError(500, 'write_failed');
}
