/**
 * Retarget existing full-server fixture tests to the composed public socket.
 * The old ServerInstance methods still stop the actual private companion.
 */
import { mock } from 'bun:test';
import { startComposedServer } from '../../src/web-server/compose-server';
import * as web from '../../src/web-server';

const binary = process.env.AAC_RUST_PUBLIC_BINARY;
if (!binary) throw new Error('AAC_RUST_PUBLIC_BINARY is required for the composed test fixture.');
const startPrivate = web.startServer;

mock.module('../../src/web-server', () => ({
  ...web,
  async startServer(options: Parameters<typeof startPrivate>[0]) {
    if (options.compositionProof) return startPrivate(options);
    const instance = await startComposedServer(options, binary);
    const server = new Proxy(instance.server, {
      get(target, property) {
        if (property === 'address') return () => instance.publicAddress;
        const value = Reflect.get(target, property, target);
        return typeof value === 'function' ? value.bind(target) : value;
      },
    });
    return { ...instance, server };
  },
}));
