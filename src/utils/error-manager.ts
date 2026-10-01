import { initUI, header, color, dim, info, errorBox } from './ui';
import { ERROR_CODES, getErrorDocUrl } from './error-codes';
import type { ErrorCode } from './error-codes';
import { getPortCheckCommand, getKillPidCommand } from './platform-commands';

/**
 * Error types with structured messages (Legacy - kept for compatibility)
 */
export const ErrorTypes = {
  NO_CLAUDE_CLI: 'NO_CLAUDE_CLI',
  MISSING_SETTINGS: 'MISSING_SETTINGS',
  INVALID_CONFIG: 'INVALID_CONFIG',
  UNKNOWN_PROFILE: 'UNKNOWN_PROFILE',
  PERMISSION_DENIED: 'PERMISSION_DENIED',
  GENERIC: 'GENERIC',
} as const;

export type ErrorType = (typeof ErrorTypes)[keyof typeof ErrorTypes];

/**
 * Enhanced error manager with context-aware messages
 */
export class ErrorManager {
  /**
   * Show error code and documentation URL
   */
  static showErrorCode(errorCode: ErrorCode): void {
    console.error(dim(`Error: ${errorCode}`));
    console.error(dim(getErrorDocUrl(errorCode)));
    console.error('');
  }

  /**
   * Show Claude CLI not found error
   */
  static async showClaudeNotFound(): Promise<void> {
    await initUI();

    console.error('');
    console.error(
      errorBox(
        'Claude CLI not found\n\n' +
          'CCS requires Claude CLI to be installed\n' +
          'and available in PATH.',
        'ERROR'
      )
    );
    console.error('');

    console.error(header('SOLUTIONS'));
    console.error('');
    console.error('  1. Install Claude CLI');
    console.error(`     ${color('https://docs.claude.com/install', 'path')}`);
    console.error('');

    // Windows-specific guidance for native installer users
    if (process.platform === 'win32') {
      console.error('  2. If you used the Windows installer, run:');
      console.error(`     ${color('claude install', 'command')}`);
      console.error(dim('     This adds Claude to your PATH'));
      console.error('');
      console.error('  3. Verify installation');
      console.error(`     ${color('Get-Command claude', 'command')}`);
      console.error('');
      console.error('  4. Custom path (if installed elsewhere)');
      console.error(`     ${color('$env:CCS_CLAUDE_PATH="C:\\path\\to\\claude.exe"', 'command')}`);
    } else {
      console.error('  2. Verify installation');
      console.error(`     ${color('command -v claude', 'command')}`);
      console.error('');
      console.error('  3. Custom path (if installed elsewhere)');
      console.error(`     ${color('export CCS_CLAUDE_PATH="/path/to/claude"', 'command')}`);
    }
    console.error('');

    this.showErrorCode(ERROR_CODES.CLAUDE_NOT_FOUND);
  }

  /**
   * Show settings file not found error
   */
  static async showSettingsNotFound(settingsPath: string): Promise<void> {
    await initUI();

    const isClaudeSettings =
      settingsPath.includes('.claude') && settingsPath.endsWith('settings.json');

    console.error('');
    console.error(errorBox('Settings file not found\n\n' + `File: ${settingsPath}`, 'ERROR'));
    console.error('');

    if (isClaudeSettings) {
      console.error('This file is auto-created when you login to Claude CLI.');
      console.error('');
      console.error(header('SOLUTIONS'));
      console.error(`  ${color(`echo '{}' > ${settingsPath}`, 'command')}`);
      console.error(`  ${color('claude /login', 'command')}`);
      console.error('');
      console.error(dim('Why: Newer Claude CLI versions require explicit login.'));
    } else {
      console.error(header('SOLUTIONS'));
      console.error('  Restore the private profile settings from a trusted backup.');
      console.error('');
      console.error(dim('Reinstalling AI Account Center does not recreate private account files.'));
    }

    console.error('');
    this.showErrorCode(ERROR_CODES.CONFIG_INVALID_PROFILE);
  }

  /**
   * Show invalid configuration error
   */
  static async showInvalidConfig(configPath: string, errorDetail: string): Promise<void> {
    await initUI();

    console.error('');
    console.error(
      errorBox(
        'Configuration invalid\n\n' + `File: ${configPath}\n` + `Issue: ${errorDetail}`,
        'ERROR'
      )
    );
    console.error('');

    console.error(header('SOLUTIONS'));
    console.error('');
    console.error(`  ${dim('# Backup corrupted file')}`);
    console.error(`  ${color(`mv ${configPath} ${configPath}.backup`, 'command')}`);
    console.error('');
    console.error(
      '  Setup and recovery: https://github.com/sittingmongoose/ai-account-center/tree/feat/activate-in-place#install-from-current-source'
    );
    console.error('');
    console.error(dim('Your profile settings will be preserved.'));
    console.error('');

    this.showErrorCode(ERROR_CODES.CONFIG_INVALID_JSON);
  }

  /**
   * Show profile not found error
   */
  static async showProfileNotFound(
    profileName: string,
    availableProfiles: string[],
    suggestions: string[] = []
  ): Promise<void> {
    await initUI();

    console.error('');
    console.error(errorBox(`Profile '${profileName}' not found`, 'ERROR'));
    console.error('');

    if (suggestions && suggestions.length > 0) {
      console.error(header('DID YOU MEAN'));
      suggestions.forEach((s) => console.error(`  ${color(s, 'command')}`));
      console.error('');
    }

    console.error(header('AVAILABLE PROFILES'));
    availableProfiles.forEach((line) => console.error(`  ${color(line, 'info')}`));
    console.error('');

    console.error(header('SOLUTIONS'));
    console.error('');
    console.error('  Use an existing profile:');
    console.error(`    ${color('ccs <profile> "your prompt"', 'command')}`);
    console.error('');
    console.error('  Create a new account profile:');
    console.error(`    ${color('ccs auth create <name>', 'command')}`);
    console.error('');
    console.error(info(`Tip: Use ${color('ccs config', 'command')} for web-based configuration`));
    console.error('');

    this.showErrorCode(ERROR_CODES.PROFILE_NOT_FOUND);
  }

  /**
   * Show permission denied error
   */
  static async showPermissionDenied(filePath: string): Promise<void> {
    await initUI();

    console.error('');
    console.error(errorBox('Permission denied\n\n' + `Cannot write to: ${filePath}`, 'ERROR'));
    console.error('');

    console.error(header('SOLUTIONS'));
    console.error('');
    console.error(`  ${dim('# Fix ownership')}`);
    console.error(`  ${color('sudo chown -R $USER ~/.ccs ~/.claude', 'command')}`);
    console.error('');
    console.error(`  ${dim('# Fix permissions')}`);
    console.error(`  ${color('chmod 755 ~/.ccs ~/.claude', 'command')}`);
    console.error('');
    console.error(`  ${dim('# Retry installation')}`);
    console.error(
      '  Setup and recovery: https://github.com/sittingmongoose/ai-account-center/tree/feat/activate-in-place#install-from-current-source'
    );
    console.error('');

    this.showErrorCode(ERROR_CODES.FS_CANNOT_WRITE_FILE);
  }

  /**
   * Show CLIProxy OAuth timeout error
   */
  static async showOAuthTimeout(provider: string): Promise<void> {
    await initUI();

    console.error('');
    console.error(
      errorBox('OAuth Timeout\n\n' + 'Authentication did not complete within 2 minutes.', 'ERROR')
    );
    console.error('');

    console.error(header('TROUBLESHOOTING'));
    console.error('  1. Check if browser opened (popup blocker?)');
    console.error('  2. Complete login in browser, then return here');
    console.error('  3. Try different browser');
    console.error('  4. Disable browser extensions temporarily');
    console.error('');

    console.error(header('FOR HEADLESS/SSH ENVIRONMENTS'));
    console.error(`  ${color(`ccs ${provider} --auth --headless`, 'command')}`);
    console.error('');
    console.error(dim('This displays manual authentication steps.'));
    console.error('');
    console.error(info(`Tip: Use ${color('ccs config', 'command')} for web-based configuration`));
    console.error('');
  }

  /**
   * Show CLIProxy port conflict error
   */
  static async showPortConflict(port: number): Promise<void> {
    await initUI();
    const isWindows = process.platform === 'win32';

    console.error('');
    console.error(
      errorBox('Port Conflict\n\n' + `CLIProxy port ${port} is already in use.`, 'ERROR')
    );
    console.error('');

    console.error(header('SOLUTIONS'));
    console.error('');
    console.error('  1. Find process using port:');
    console.error(`     ${color(getPortCheckCommand(port), 'command')}`);
    console.error('');
    console.error('  2. Kill the process:');
    if (isWindows) {
      console.error(
        `     ${color(`taskkill /F /PID <PID>`, 'command')}  (replace <PID> with actual ID)`
      );
    } else {
      console.error(`     ${color(getKillPidCommand(12345).replace('12345', '<PID>'), 'command')}`);
    }
    console.error('');
    console.error('  3. Auto-fix: Run:');
    console.error(`     ${color('ccs doctor --fix', 'command')}`);
    console.error('');
  }

  /**
   * Show CLIProxy binary download failure error
   */
  static async showBinaryDownloadFailed(url: string, error: string): Promise<void> {
    await initUI();

    console.error('');
    console.error(errorBox('Binary Download Failed\n\n' + `Error: ${error}`, 'ERROR'));
    console.error('');

    console.error(header('TROUBLESHOOTING'));
    console.error('  1. Check internet connection');
    console.error('  2. Check firewall/proxy settings');
    console.error('  3. Try again in a few minutes');
    console.error('');

    console.error(header('MANUAL DOWNLOAD'));
    console.error(`  URL: ${color(url, 'path')}`);
    console.error(`  Save to: ${color('~/.ccs/bin/cliproxyapi', 'path')}`);
    console.error(`  ${color('chmod +x ~/.ccs/bin/cliproxyapi', 'command')}`);
    console.error('');
  }

  /**
   * Show CLIProxy authentication required error
   */
  static async showAuthRequired(provider: string): Promise<void> {
    await initUI();

    const displayName = provider.charAt(0).toUpperCase() + provider.slice(1);

    console.error('');
    console.error(errorBox(`${displayName} authentication required`, 'ERROR'));
    console.error('');

    console.error(header('TO AUTHENTICATE'));
    console.error(`  ${color(`ccs ${provider} --auth`, 'command')}`);
    console.error('');
    console.error(dim('This will open a browser for OAuth login.'));
    console.error(dim('After login, you can use the profile normally.'));
    console.error('');
    console.error(info(`Tip: Use ${color('ccs config', 'command')} for web-based configuration`));
    console.error('');
  }
}
