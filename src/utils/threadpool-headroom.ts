/**
 * Thread Pool Headroom
 *
 * Node runs asynchronous file calls and DNS lookups on one small shared thread
 * pool (four threads by default). A file call on a hard-mounted share that
 * stopped answering holds its thread until the share returns, and the usage
 * readers' deadlines settle the caller but cannot free that thread. Four such
 * calls would stall every file read and every provider request's DNS lookup.
 *
 * libuv sizes the pool on first use, so this module must be the entry point's
 * first import. An explicit UV_THREADPOOL_SIZE is kept.
 */

export const THREADPOOL_HEADROOM_SIZE = 16;

if (!process.env.UV_THREADPOOL_SIZE) {
  process.env.UV_THREADPOOL_SIZE = String(THREADPOOL_HEADROOM_SIZE);
}
