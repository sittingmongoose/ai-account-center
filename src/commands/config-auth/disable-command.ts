/**
 * Config Auth Disable Command
 *
 * Disable dashboard authentication with confirmation.
 */

import { InteractivePrompt } from '../../utils/prompt';

import { initUI, header, ok, info, warn, dim } from '../../utils/ui';
import { getDashboardAuthConfig, mutateConfig } from '../../config/config-loader-facade';

/**
 * Handle disable command - disable auth with confirmation
 */
export async function handleDisable(): Promise<void> {
  await initUI();

  console.log('');
  console.log(header('Disable Dashboard Auth'));
  console.log('');

  const config = getDashboardAuthConfig();

  // Check if already disabled
  if (!config.enabled) {
    console.log(info('Dashboard authentication is already disabled.'));
    console.log('');
    console.log(dim('    To enable: ccs config auth setup'));
    console.log('');
    return;
  }

  // Check for ENV override
  if (process.env.CCS_DASHBOARD_AUTH_ENABLED) {
    console.log(warn('CCS_DASHBOARD_AUTH_ENABLED environment variable is set.'));
    console.log(info('Disabling in config.yaml, but ENV var will still override.'));
    console.log('');
  }

  // Confirm before disabling
  console.log(warn('This will disable login protection for the dashboard.'));
  console.log(info('Anyone with network access will be able to view the dashboard.'));
  console.log('');

  const confirmed = await InteractivePrompt.confirm('Disable authentication?', {
    default: false, // Safe default
  });

  if (!confirmed) {
    console.log('');
    console.log(info('Cancelled. Authentication remains enabled.'));
    console.log('');
    return;
  }

  // Disable auth
  mutateConfig((fullConfig) => {
    const lifetime = fullConfig.dashboard_auth?.session_lifetime_days;
    fullConfig.dashboard_auth = {
      enabled: false,
      username: fullConfig.dashboard_auth?.username ?? '',
      password_hash: fullConfig.dashboard_auth?.password_hash ?? '',
      session_timeout_hours: fullConfig.dashboard_auth?.session_timeout_hours ?? 24,
      ...(typeof lifetime === 'number' ? { session_lifetime_days: lifetime } : {}),
    };
  });

  console.log('');
  console.log(ok('Dashboard authentication disabled'));
  console.log('');
  console.log(info('Credentials preserved - re-enable with: ccs config auth setup'));
  console.log('');
}
