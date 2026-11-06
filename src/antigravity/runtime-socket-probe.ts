import { lstat } from 'node:fs/promises';
import { connect } from 'node:net';
import { dirname } from 'node:path';

export type RuntimeSocketState = 'stopped' | 'listening' | 'unknown';

/**
 * Whether a resident runtime listens on its private socket, without sending a
 * request. 'stopped' only when nothing can be listening: the socket (or its
 * folder) is absent, or a leftover socket file refuses the connection.
 * Anything unsafe or unclear is 'unknown', never 'stopped'.
 */
export async function probeRuntimeSocket(
  socketPath: string,
  timeoutMs = 2000
): Promise<RuntimeSocketState> {
  if (process.platform !== 'linux' || typeof process.getuid !== 'function') return 'unknown';
  const uid = process.getuid();
  try {
    const directory = await lstat(dirname(socketPath));
    if (!directory.isDirectory() || directory.uid !== uid) return 'unknown';
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'stopped' : 'unknown';
  }
  try {
    const info = await lstat(socketPath);
    if (!info.isSocket() || info.uid !== uid) return 'unknown';
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'stopped' : 'unknown';
  }
  return new Promise<RuntimeSocketState>((resolve) => {
    const socket = connect(socketPath);
    let done = false;
    const finish = (state: RuntimeSocketState) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(state);
    };
    const timer = setTimeout(() => finish('unknown'), timeoutMs);
    socket.on('connect', () => finish('listening'));
    socket.on('error', (error: NodeJS.ErrnoException) =>
      finish(error.code === 'ECONNREFUSED' || error.code === 'ENOENT' ? 'stopped' : 'unknown')
    );
  });
}
