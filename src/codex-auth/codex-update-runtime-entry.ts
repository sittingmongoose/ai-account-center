/**
 * Entry of the self-contained Codex stop/start runtime that the app-update
 * bridge (scripts/app-updates/app_update_codex.cjs) loads on a Linux host
 * without an AAC install. scripts/build-codex-update-runtime.js bundles it,
 * with ws and proper-lockfile inlined, into
 * dist/app-updates/app_update_codex_runtime.cjs.
 *
 * Only the process runtime and the lock library are exported: account
 * activation (auth.json replacement) is never part of this bundle, and
 * scripts/verify-bundle.js rejects a bundle that contains it.
 */
import * as lockfile from 'proper-lockfile';

export { createCodexActivationRuntime } from './codex-activation-runtime';
export { lockfile };
