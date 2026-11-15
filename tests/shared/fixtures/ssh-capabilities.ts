/** Probe before suites replace the remote stdin process seam; -G -F none never connects. */
import { sshOptions } from '../../../src/web-server/services/ssh-policy';
try {
  await sshOptions('fixture');
} catch {
  // Pure unit tests also run on machines without an SSH client. SSH callers still
  // see a cached capability failure; no capabilities are fabricated for them.
}
