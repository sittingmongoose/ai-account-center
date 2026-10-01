/**
 * Current AI Account Center bar command and installation guards.
 *
 * The retired upstream suite covered a floating release downloader and archive
 * installer that this product no longer uses. These fixture modules exercise
 * the packaged native installer, strict branded/legacy app ownership, reversible
 * uninstall, read-only help/version, and nonce-authenticated server identity.
 * Every HTTP endpoint is an owned ephemeral fixture; no default ports are probed.
 */
import '../../../src/commands/bar/__tests__/packaged-install.test';
import '../../../src/commands/bar/__tests__/native-app-paths.test';
import '../../../src/commands/bar/__tests__/cli-read-only.test';
import '../../../src/commands/bar/__tests__/auth-probe.test';
