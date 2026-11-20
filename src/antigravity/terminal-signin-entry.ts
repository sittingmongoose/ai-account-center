import { handleAntigravityCommand } from '../commands/antigravity-command';

const args = process.argv.slice(2);
if (args[0] !== 'antigravity' || args[1] !== 'signin' || args.length !== 3) {
  process.exitCode = 1;
} else {
  void handleAntigravityCommand(args.slice(1)).then(
    (status) => {
      process.exitCode = status;
    },
    () => {
      console.error('[X] Antigravity sign-in could not be completed safely.');
      process.exitCode = 1;
    }
  );
}
