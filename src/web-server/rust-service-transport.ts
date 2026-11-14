/** Bounded HTTP over a private Unix socket. Never copy caller headers into this request. */
import http from 'http';
import { NetworkError } from '../errors/error-types';

export interface VerifiedRustPrincipal {
  kind: 'session' | 'device' | 'bar' | 'hub';
  id: string;
  role: 'public' | 'admin' | 'device' | 'hub';
}

export interface RustConnection {
  socketPath: string;
  secret: string;
  bootId: string;
}

export interface RustReply {
  status: number;
  body: unknown;
}

export function requestRust(
  connection: RustConnection,
  route: string,
  principal: VerifiedRustPrincipal,
  active: Set<http.ClientRequest>
): Promise<RustReply> {
  return new Promise((resolve, reject) => {
    const request = http.request({
      socketPath: connection.socketPath,
      path: route,
      method: 'GET',
      agent: false,
      headers: {
        'x-aac-boot-secret': connection.secret,
        'x-aac-principal': JSON.stringify(principal),
      },
    });
    active.add(request);
    const timer = setTimeout(() => request.destroy(new NetworkError('Rust service deadline')), 500);
    request.once('close', () => {
      clearTimeout(timer);
      active.delete(request);
    });
    request.once('error', reject);
    request.once('response', (response) => {
      const chunks: Buffer[] = [];
      let size = 0;
      response.on('data', (chunk: Buffer) => {
        size += chunk.length;
        if (size > 64 * 1024) request.destroy(new NetworkError('Rust service response cap'));
        else chunks.push(chunk);
      });
      response.once('error', reject);
      response.once('end', () => {
        if (response.headers['x-aac-service-boot'] !== connection.bootId) {
          reject(new NetworkError('Rust service boot mismatch'));
          return;
        }
        try {
          resolve({
            status: response.statusCode ?? 503,
            body: JSON.parse(Buffer.concat(chunks).toString('utf8')),
          });
        } catch {
          reject(new NetworkError('Rust service invalid response'));
        }
      });
    });
    request.end();
  });
}
