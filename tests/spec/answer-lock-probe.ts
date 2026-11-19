/**
 * G16b: the TypeScript side of the cross-process writer-lock test
 * (crates/aac-status/tests/writer_lock_cross.rs). Never run against a real store.
 *
 *   bun tests/spec/answer-lock-probe.ts <store> hold   # take the lock, print "held <pid>", keep it until stdin closes
 *   bun tests/spec/answer-lock-probe.ts <store> try    # print "gained", "kept" or "busy <message>"
 *   bun tests/spec/answer-lock-probe.ts <store> crash  # take the lock and exit without releasing it
 */
import { holdWriterLock, WriterBusyError } from '../../src/web-server/status/writer-lock';

const [store, mode] = process.argv.slice(2);
if (!store || !mode) throw new Error('usage: answer-lock-probe.ts <store> hold|try|crash');
try {
  const gained = holdWriterLock(store);
  if (mode === 'hold') {
    process.stdout.write(`held ${process.pid}\n`);
    process.stdin.resume();
    process.stdin.on('end', () => process.exit(0));
  } else if (mode === 'crash') {
    process.stdout.write(`held ${process.pid}\n`);
    // Exit without the release hook: a crashed holder.
    process.reallyExit(0);
  } else {
    process.stdout.write(gained ? 'gained\n' : 'kept\n');
  }
} catch (error) {
  if (!(error instanceof WriterBusyError)) throw error;
  process.stdout.write(`busy ${error.message}\n`);
}
