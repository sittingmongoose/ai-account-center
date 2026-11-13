import { color, dim, header, initUI, subheader } from '../../utils/ui';

export async function showHelp(): Promise<void> {
  await initUI();
  console.log('');
  console.log(header('AI Account Center - macOS bar'));
  console.log('');
  console.log(subheader('Usage:'));
  console.log(`  ${color('ai-account-center bar', 'command')} [command] [options]`);
  console.log('');

  const sections: [string, [string, string][]][] = [
    [
      'Commands:',
      [
        [
          'install',
          'Build and install the macOS app from the native sources included in this package',
        ],
        ['uninstall', 'Remove the owned app while preserving connection and account settings'],
        ['version', 'Show CLI and installed app versions'],
      ],
    ],
    [
      'Options:',
      [
        ['--help, -h', 'Show this help message'],
        ['--version', 'Show CLI and installed app versions'],
      ],
    ],
    [
      'Install options:',
      [
        ['--launch', 'Launch the app after installation'],
        ['--no-launch', 'Install without launching'],
      ],
    ],
    [
      'Examples:',
      [
        ['ai-account-center bar', 'Open the paired native app'],
        [
          'ai-account-center bar install --launch',
          'Build, install and launch the packaged native app',
        ],
        ['ai-account-center bar install --no-launch', 'Build and install without launching'],
        ['ai-account-center bar version', 'Show CLI and installed native app versions'],
      ],
    ],
  ];
  for (const [title, rows] of sections) {
    console.log(subheader(title));
    const width = Math.max(...rows.map(([command]) => command.length));
    for (const [command, description] of rows) {
      console.log(`  ${color(command.padEnd(width + 2), 'command')} ${description}`);
    }
    console.log('');
  }
  console.log(dim('  macOS builds require Xcode or Command Line Tools with Swift.'));
  console.log(
    dim('  Installation preserves the saved connection, startup preference and legacy app path.')
  );
  console.log(dim('  The native app uses its saved dashboard address and login.'));
  console.log(dim('  The compatible `ccs bar` command uses the same installer.'));
  console.log(dim('  On Windows, use the installed AI Account Center tray app.'));
  console.log('');
}
