/** Open the owned native app using its saved dashboard connection. */
import { ConfigError } from '../../errors/error-types';
import { getMacAppPaths, resolveOwnedMacApp } from './native-app-paths';
export interface LaunchDeps {
  getPlatform: () => NodeJS.Platform;
  getAppsDir: () => string;
  openApp: (app: string) => Promise<void>;
}
export async function handleBarLaunch(
  args: string[],
  deps: Partial<LaunchDeps> = {}
): Promise<void> {
  if (args.length) {
    console.error('[X] Usage: ai-account-center bar');
    process.exitCode = 1;
    return;
  }
  if ((deps.getPlatform?.() ?? process.platform) !== 'darwin') {
    console.error('[X] Open the installed native tray app on macOS or Windows.');
    process.exitCode = 1;
    return;
  }
  try {
    const appsDir = deps.getAppsDir?.() ?? getMacAppPaths().appsDir;
    const app = resolveOwnedMacApp(appsDir);
    if (!app) throw new ConfigError('App not installed. Run ai-account-center bar install.');
    const openApp =
      deps.openApp ??
      (async (target: string) => {
        const open = (await import('open')).default;
        await open(target);
      });
    await openApp(app.path);
    console.log('[OK] AI Account Center opened.');
  } catch (error) {
    console.error('[X] ' + (error instanceof Error ? error.message : String(error)));
    process.exitCode = 1;
  }
}
