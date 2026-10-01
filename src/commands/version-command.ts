import { initUI } from '../utils/ui';
import { getActiveConfigPath, getCcsDir } from '../utils/config-manager';
import { getVersion } from '../utils/version';

export async function handleVersionCommand(): Promise<void> {
  await initUI();
  console.log(`AI Account Center v${getVersion()}`);
  console.log(`Installation: ${process.argv[1] || '(not found)'}`);
  console.log(`Account configuration: ${getCcsDir()}`);
  console.log(`Config: ${getActiveConfigPath()}`);
  console.log('License: MIT; derived from CCS by Tam Nhu Tran (Kai).');
  console.log("Run 'ai-account-center --help' for usage information.");
}
