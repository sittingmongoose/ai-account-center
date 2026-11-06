'use strict';
// Remove the WebAssembly "name" custom section (function names for debugging, about 90% of a release build).
// Pure: bytes in, bytes out. Every other section, including target_features, is kept byte for byte.
const fs = require('fs');

function leb(bytes, at) {
  let value = 0;
  let shift = 0;
  for (;;) {
    if (at >= bytes.length) throw new Error('truncated LEB128');
    const b = bytes[at++];
    value += (b & 0x7f) * 2 ** shift;
    shift += 7;
    if (b < 0x80) return [value, at];
  }
}

function stripNameSection(bytes) {
  if (bytes.length < 8 || bytes.readUInt32LE(0) !== 0x6d736100) throw new Error('not a WebAssembly module');
  const parts = [bytes.subarray(0, 8)];
  let at = 8;
  while (at < bytes.length) {
    const start = at;
    const id = bytes[at++];
    const [size, body] = leb(bytes, at);
    const end = body + size;
    if (end > bytes.length) throw new Error('truncated section');
    let keep = true;
    if (id === 0) {
      const [nameLength, nameAt] = leb(bytes, body);
      keep = bytes.subarray(nameAt, nameAt + nameLength).toString('utf8') !== 'name';
    }
    if (keep) parts.push(bytes.subarray(start, end));
    at = end;
  }
  return Buffer.concat(parts);
}

module.exports = { stripNameSection };

if (require.main === module) {
  const file = process.argv[2];
  const out = stripNameSection(fs.readFileSync(file));
  fs.writeFileSync(file, out);
  console.log(`${file}: ${out.length} bytes`);
}
