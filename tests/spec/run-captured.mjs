// Capture a spec runner's full output without losing the failing step (TOOL-ISSUES catalog-astra-T1).
// Pipe capture loses a large corpus diff: Node's default buffer surfaces ENOBUFS (wave 63: about 14 MB),
// and Bun 1.4.4's spawnSync silently truncates to a few hundred KB regardless of maxBuffer. Streaming to
// a file keeps every byte under both runtimes; a failure reports the failing step first, then a bounded
// tail. The report keeps at most MAX_BUFFER bytes (the same bound the parity readers use).
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const MAX_BUFFER = 64 * 1024 * 1024;
const TAIL_LINES = 120;
const TAIL_CHARS = 64 * 1024;

/** The innermost failing command. run.mjs execs each step with stdio inherit, so the failing step's own
 * "Command failed: <program> <args>" line lands in the captured output (Node prints it as "Error: Command
 * failed: ..." on an uncaught throw); the last marker is the step that failed. */
export function failingStep(output) {
  const text = String(output ?? '');
  const idx = text.lastIndexOf('Command failed: ');
  if (idx < 0) return null;
  const line = text.slice(idx + 'Command failed: '.length).split('\n')[0].trim();
  return line || null;
}

/** Run to completion with stdout and stderr streamed to a temp file, never a JS pipe buffer. Success
 * returns the captured output (at most MAX_BUFFER bytes, the last ones). Failure throws an error naming
 * the failing step first, then a bounded tail; error.stdout holds the same bounded output. */
export function runCaptured(program, args, opts = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-spec-capture-'));
  const outPath = path.join(dir, 'output.log');
  let result;
  let output = '';
  let total = 0;
  try {
    const fd = fs.openSync(outPath, 'w');
    try {
      result = spawnSync(program, args, { ...opts, stdio: ['ignore', fd, fd] });
    } finally {
      fs.closeSync(fd);
    }
    total = fs.statSync(outPath).size;
    const keep = Math.min(total, MAX_BUFFER);
    const readFd = fs.openSync(outPath, 'r');
    try {
      const buf = Buffer.alloc(keep);
      fs.readSync(readFd, buf, 0, keep, total - keep);
      output = buf.toString('utf8');
    } finally {
      fs.closeSync(readFd);
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  if (!result.error && result.status === 0 && !result.signal) return output;
  const step = failingStep(output);
  let tail = output.split('\n').slice(-TAIL_LINES).join('\n');
  if (tail.length > TAIL_CHARS) tail = tail.slice(-TAIL_CHARS);
  const head = step
    ? `failing step: ${step}`
    : 'spec run failed; no failing step marker in the output; last output below';
  const sizeNote = total > MAX_BUFFER ? ` (output was ${total} bytes; kept the last ${MAX_BUFFER})` : '';
  const error = new Error(`${head}${sizeNote}\n--- last ${TAIL_LINES} lines of output ---\n${tail}`);
  error.status = result.status;
  error.signal = result.signal;
  error.code = result.error ? result.error.code : result.status;
  error.stdout = output;
  throw error;
}
