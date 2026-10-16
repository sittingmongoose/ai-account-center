/** Offline Node probe. Compile src first, then: node <this file> <build dir> [decoded GiB]. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Readable } = require('node:stream');
const { pipeline } = require('node:stream/promises');
const { createZstdCompress } = require('node:zlib');

async function main() {
  const build = path.resolve(process.argv[2] || path.join(__dirname, '../../../dist'));
  const gib = Number(process.argv[3] || 12);
  assert(Number.isInteger(gib) && gib >= 1 && gib <= 12);
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-r5-zst-probe-'));
  process.env.CCS_HOME = root;
  delete process.env.CCS_DIR;
  const { collectAccountActivity } = require(
    path.join(build, 'web-server/usage/account-activity-collector.js')
  );
  const { LargeCodexReadQueue, spawnLargeCodexReader } = require(
    path.join(build, 'web-server/usage/worker-client.js')
  );
  const file = path.join(root, 'codex/sessions/rollout-01234567-1234-1234-1234-0123456789ab.jsonl');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const timestamp = new Date().toISOString();
  const record = (n) =>
    JSON.stringify({
      timestamp,
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: n * 100,
            cached_input_tokens: n * 20,
            output_tokens: n * 10,
          },
        },
      },
    }) + '\n';
  const prefix =
    JSON.stringify({
      type: 'session_meta',
      payload: {
        id: '01234567-1234-1234-1234-0123456789ab',
        originator: 'codex_cli_rs',
      },
    }) +
    '\n' +
    JSON.stringify({ type: 'turn_context', payload: { model: 'gpt-5.4' } }) +
    '\n' +
    record(1);
  const paddingStart = '{"type":"response_item","payload":"';
  const suffix = '"}\n' + record(2);
  const request = {
    kind: 'codex',
    codexHome: path.join(root, 'codex'),
    cacheDir: path.join(root, 'cache'),
  };
  const options = { minDate: Date.now() - 31 * 86400000, cacheDir: request.cacheDir };
  const collect = (extra = {}) => collectAccountActivity(request, { ...options, ...extra });
  let queue;
  let sampler;
  try {
    // The uncompressed reference has identical usage records, with the ignored payload empty.
    // The giant fixture itself is streamed into one zstd frame; decoded bytes never touch disk.
    fs.writeFileSync(file, prefix + paddingStart + suffix);
    const reference = await collect({ cacheDir: path.join(root, 'reference') });
    fs.unlinkSync(file);
    const chunk = Buffer.alloc(1024 * 1024, 'x');
    const fixtureStart = performance.now();
    await pipeline(
      Readable.from(
        (async function* () {
          yield Buffer.from(prefix + paddingStart);
          for (let i = 0; i < gib * 1024; i++) yield chunk;
          yield Buffer.from(suffix);
        })()
      ),
      createZstdCompress(),
      fs.createWriteStream(`${file}.zst`)
    );
    const fixtureMs = performance.now() - fixtureStart;
    console.log(
      JSON.stringify({
        phase: 'fixture',
        node: process.version,
        decodedGiB: gib,
        compressedBytes: fs.statSync(`${file}.zst`).size,
        fixtureMs: Math.round(fixtureMs),
      })
    );
    const first = await collect({ maxBytesPerFile: 512 * 1024 * 1024 });
    assert(first.scan.readBytes > 64 * 1024 * 1024 && !first.scan.complete);
    let spec;
    for (let pass = 0; pass < 30 && !spec; pass++) {
      const result = await collect({ budgetMs: 20 });
      spec = result.scan?.largeCodexReads?.[0];
    }
    assert(spec, 'reduced normal passes must reach the paused state');
    const before = fs.readFileSync(spec.checkpoint, 'utf8');
    let backgroundMs;
    let readerDone = false;
    let peakRss = process.memoryUsage().rss;
    const baselineRss = peakRss;
    sampler = setInterval(() => {
      peakRss = Math.max(peakRss, process.memoryUsage().rss);
    }, 20);
    queue = new LargeCodexReadQueue(async (...args) => {
      const start = performance.now();
      try {
        return await spawnLargeCodexReader(...args);
      } finally {
        backgroundMs = performance.now() - start;
        readerDone = true;
      }
    });
    queue.enqueue([spec]);
    const deadline = performance.now() + 905000;
    while (!fs.existsSync(`${spec.checkpoint}.large-complete`) && performance.now() < deadline) {
      if (readerDone && !queue.isRunning([spec])) break;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    assert(fs.existsSync(`${spec.checkpoint}.large-complete`), 'background read must finish');
    assert.equal(
      fs.readFileSync(spec.checkpoint, 'utf8'),
      before,
      'handoff must preserve paused checkpoint'
    );
    const complete = await collect();
    assert.equal(complete.scan.complete, true);
    assert.equal(complete.scan.detail, undefined);
    assert.deepEqual(complete.hourly, reference.hourly);
    assert.deepEqual(complete.session, reference.session);
    assert.equal(complete.eventCount, reference.eventCount);
    const warm = await collect();
    assert.equal(warm.scan.readBytes, 0);
    assert.equal(warm.eventCount, 2);
    console.log(
      JSON.stringify({
        phase: 'background-complete',
        node: process.version,
        decodedGiB: gib,
        backgroundMs: Math.round(backgroundMs),
        baselineRssMiB: Math.round(baselineRss / 1024 ** 2),
        backgroundPeakRssMiB: Math.round(peakRss / 1024 ** 2),
        processPeakRssMiB: Math.round(process.resourceUsage().maxRSS / 1024),
        complete: complete.scan.complete,
        eventCount: complete.eventCount,
        equalToPlain: true,
      })
    );
  } finally {
    clearInterval(sampler);
    await queue?.shutdown();
    fs.rmSync(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
