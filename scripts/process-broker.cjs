'use strict';
// Private child_process transport. No socket, shell, persistent state or diagnostics.
// Forking commands here copies this small process's page tables, not the dashboard heap.
const { spawn } = require('child_process');
// Pin the owner before startup: it may die before this process loads its script.
const parentPid = Number(process.argv[2] ?? process.ppid);
const jobs = new Map();
let bufferedBytes = 0;
const send = (packet) => {
  if (process.connected) process.send(packet, () => {});
};
const signal = (child, name) => {
  try {
    if (process.platform === 'win32') child.kill(name);
    else process.kill(-child.pid, name);
  } catch {
    /* Only this job's owned process group. */
  }
};
const shutdown = () => {
  clearInterval(parentWatchdog);
  for (const job of jobs.values()) signal(job.child, 'SIGKILL');
  process.exit(0);
};
const checkParent = () => {
  if (!process.connected || process.ppid !== parentPid) return shutdown();
  try {
    process.kill(parentPid, 0);
  } catch (error) {
    if (error.code === 'ESRCH') shutdown();
  }
};
// Independent of IPC notification delivery, including an abrupt owner crash.
const parentWatchdog = setInterval(checkParent, 250);
parentWatchdog.unref();
process.once('disconnect', shutdown);
process.once('SIGTERM', shutdown);
process.once('SIGINT', shutdown);
process.once('exit', () => {
  for (const job of jobs.values()) signal(job.child, 'SIGKILL');
});
process.on('message', (packet) => {
  if (packet?.type === 'stop') {
    shutdown();
    return;
  }
  if (packet?.type === 'cancel') {
    const job = jobs.get(packet.id);
    if (job) job.expire();
    return;
  }
  if (packet?.type === 'deadline') {
    jobs.get(packet.id)?.extendDeadline(packet.deadlineAt);
    return;
  }
  if (packet?.type !== 'run' || !Number.isSafeInteger(packet.id)) return;
  const { id, file, args, options } = packet;
  const refuse = (timedOut = false) =>
    send({
      type: 'result',
      id,
      result: { code: null, stdout: '', timedOut, missing: false, failed: true },
    });
  if (
    jobs.size >= 64 ||
    typeof file !== 'string' ||
    !Array.isArray(args) ||
    !args.every((arg) => typeof arg === 'string') ||
    !options ||
    typeof options.input !== 'string' ||
    Buffer.byteLength(options.input) > 4 * 1024 * 1024 ||
    !Number.isFinite(options.timeoutMs) ||
    options.timeoutMs <= 0 ||
    options.timeoutMs > 300000 ||
    !Number.isFinite(options.maxBytes) ||
    options.maxBytes <= 0 ||
    options.maxBytes > 16 * 1024 * 1024 ||
    !Number.isFinite(options.graceMs) ||
    options.graceMs < 0 ||
    options.graceMs > 30000 ||
    !Number.isFinite(packet.deadlineAt) ||
    !Number.isFinite(packet.hardDeadlineAt) ||
    packet.deadlineAt > packet.hardDeadlineAt
  ) {
    refuse();
    return;
  }
  // IPC queueing consumes the caller's lifetime too. Never launch stale work.
  if (Date.now() >= packet.deadlineAt) {
    refuse(true);
    return;
  }
  let child;
  try {
    child = spawn(file, args, {
      cwd: options.cwd,
      env: options.env,
      shell: false,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
      detached: process.platform !== 'win32',
    });
  } catch {
    refuse();
    return;
  }
  let chunks = [];
  let bytes = 0;
  let retainedBytes = 0;
  let stderrBytes = 0;
  let settled = false;
  let timedOut = false;
  let forceTimer;
  let deadlineTimer;
  let deadlineAt = packet.deadlineAt;
  const finish = (failed, code = null, missing = false) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    clearTimeout(forceTimer);
    clearTimeout(deadlineTimer);
    // Kill descendants too: a finished wrapper must not leave owned helpers behind.
    signal(child, 'SIGKILL');
    jobs.delete(id);
    const stdout = failed ? '' : Buffer.concat(chunks).toString('utf8');
    bufferedBytes -= retainedBytes;
    chunks = [];
    send({
      type: 'result',
      id,
      result: { code: failed && code === 0 ? null : code, stdout, timedOut, missing, failed },
    });
  };
  const timer = setTimeout(() => {
    timedOut = true;
    signal(child, 'SIGTERM');
    forceTimer = setTimeout(() => finish(true), options.graceMs);
  }, options.timeoutMs);
  const expire = () => {
    timedOut = true;
    finish(true);
  };
  const armDeadline = () => {
    clearTimeout(deadlineTimer);
    deadlineTimer = setTimeout(expire, Math.max(0, deadlineAt - Date.now()));
  };
  armDeadline();
  jobs.set(id, {
    child,
    expire,
    extendDeadline(value) {
      if (!Number.isFinite(value)) return;
      // Shared waiters cannot restart or extend the original command's lifetime.
      const next = Math.min(value, packet.hardDeadlineAt);
      if (next <= deadlineAt) return;
      deadlineAt = next;
      armDeadline();
    },
  });
  send({ type: 'started', id, pid: child.pid });
  child.once('error', (error) => finish(true, null, error.code === 'ENOENT'));
  child.stdin.once('error', () => finish(true));
  child.stdout.on('data', (chunk) => {
    if (settled) return;
    bytes += chunk.length;
    if (bytes > options.maxBytes || bufferedBytes + chunk.length > 32 * 1024 * 1024) {
      finish(true);
      return;
    }
    chunks.push(chunk);
    retainedBytes += chunk.length;
    bufferedBytes += chunk.length;
  });
  // Drain, count and discard stderr. It is never sent to the server or logs.
  child.stderr.on('data', (chunk) => {
    stderrBytes += chunk.length;
    if (stderrBytes > options.maxBytes) finish(true);
  });
  child.once('close', (code) => finish(timedOut || code !== 0, code));
  child.stdin.end(options.input);
});
checkParent();
send({ type: 'ready' });
