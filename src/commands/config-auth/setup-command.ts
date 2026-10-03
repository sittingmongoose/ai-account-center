/**
 * Config Auth Setup Command
 *
 * Interactive wizard for configuring dashboard authentication.
 * Prompts for username and password, hashes password with bcrypt,
 * and saves to config.yaml.
 */

import bcrypt from 'bcrypt';
import { InteractivePrompt } from '../../utils/prompt';

import { initUI, header, subheader, ok, fail, info, warn, dim } from '../../utils/ui';
import type { AuthSetupResult } from './types';
import { mutateConfig } from '../../config/config-loader-facade';

const BCRYPT_ROUNDS = 10;
const MIN_PASSWORD_LENGTH = 8;

/**
 * Validate username (non-empty, alphanumeric with underscores)
 */
function validateUsername(value: string): string | null {
  if (!value || value.trim().length === 0) {
    return 'Username cannot be empty';
  }
  if (!/^[a-zA-Z][a-zA-Z0-9_-]*$/.test(value)) {
    return 'Username must start with letter, contain only letters/numbers/underscores/hyphens';
  }
  if (value.length < 3) {
    return 'Username must be at least 3 characters';
  }
  return null;
}

/**
 * Handle setup command - interactive wizard
 */
export async function handleSetup(): Promise<AuthSetupResult> {
  await initUI();

  console.log('');
  console.log(header('Dashboard Auth Setup'));
  console.log('');
  console.log(info('Configure username and password for dashboard access.'));
  console.log(dim('    Password will be hashed with bcrypt before storage.'));
  console.log('');

  // Check for ENV overrides
  if (
    process.env.CCS_DASHBOARD_AUTH_ENABLED ||
    process.env.CCS_DASHBOARD_USERNAME ||
    process.env.CCS_DASHBOARD_PASSWORD_HASH
  ) {
    console.log(warn('Environment variables detected - they will override config values:'));
    if (process.env.CCS_DASHBOARD_AUTH_ENABLED) {
      console.log(`    CCS_DASHBOARD_AUTH_ENABLED=${process.env.CCS_DASHBOARD_AUTH_ENABLED}`);
    }
    if (process.env.CCS_DASHBOARD_USERNAME) {
      console.log(`    CCS_DASHBOARD_USERNAME=${process.env.CCS_DASHBOARD_USERNAME}`);
    }
    if (process.env.CCS_DASHBOARD_PASSWORD_HASH) {
      console.log('    CCS_DASHBOARD_PASSWORD_HASH=***');
    }
    console.log('');
  }

  try {
    // Prompt for username
    console.log(subheader('Username'));
    const username = await InteractivePrompt.input('Enter username', {
      validate: validateUsername,
    });

    console.log('');

    // Prompt for password
    console.log(subheader('Password'));
    console.log(dim(`    Minimum ${MIN_PASSWORD_LENGTH} characters`));
    const password = await InteractivePrompt.password('Enter password');

    // Validate password length
    if (password.length < MIN_PASSWORD_LENGTH) {
      console.log('');
      console.log(fail(`Password must be at least ${MIN_PASSWORD_LENGTH} characters`));
      return { success: false, error: 'Password too short' };
    }

    // Confirm password
    const confirmPassword = await InteractivePrompt.password('Confirm password');

    if (password !== confirmPassword) {
      console.log('');
      console.log(fail('Passwords do not match'));
      return { success: false, error: 'Password mismatch' };
    }

    console.log('');
    console.log(info('Hashing password...'));

    // Hash password
    const passwordHash = await bcrypt.hash(password, BCRYPT_ROUNDS);

    const config = mutateConfig((currentConfig) => {
      const lifetime = currentConfig.dashboard_auth?.session_lifetime_days;
      currentConfig.dashboard_auth = {
        enabled: true,
        username,
        password_hash: passwordHash,
        session_timeout_hours: currentConfig.dashboard_auth?.session_timeout_hours ?? 24,
        ...(typeof lifetime === 'number' ? { session_lifetime_days: lifetime } : {}),
      };
    });

    console.log('');
    console.log(ok('Dashboard authentication configured'));
    console.log('');
    console.log(info('Settings saved to ~/.ccs/config.yaml'));
    console.log(info(`Username: ${username}`));
    console.log(
      info(`Session timeout: ${config.dashboard_auth?.session_timeout_hours ?? 24} hours`)
    );
    console.log('');
    console.log(dim('    Start dashboard: ccs config'));
    console.log(dim('    Show status: ccs config auth show'));
    console.log(dim('    Disable auth: ccs config auth disable'));
    console.log('');

    return { success: true, username };
  } catch (error) {
    const err = error as Error;
    console.log('');
    console.log(fail(`Setup failed: ${err.message}`));
    return { success: false, error: err.message };
  }
}
