#!/usr/bin/env bun
// D7 Home: the front door's envelope for test tools that run in Node (generate.mjs, fuzz.mjs, the browser kit's and
// the device proxy's Rust builder). The envelope is the shared M3 one (`rustReadInput`), which loads only where TS
// modules do, so these tools ask Bun:
//
//   bun tests/spec/views/home/envelope.ts presentations   stdin [{ctx, dashboard, profiles, antigravity}, ...]
//                                                          stdout [presentation, ...]
//   bun tests/spec/views/home/envelope.ts front-door      stdin {contextHeader, platform, dashboard, profiles,
//                                                          antigravity}; stdout {envelope} (null when the page's
//                                                          context is unreadable or the profiles are missing)
import { homeReadInput, parseHomeContext, type HomeContext } from '../../../../src/web-server/views/home-context';

type Inputs = { ctx: HomeContext; dashboard: unknown; profiles: unknown; antigravity: unknown };

const presentationOf = (input: Inputs): unknown =>
  (homeReadInput(input.ctx, input.dashboard, input.profiles, input.antigravity) as { presentation: unknown })
    .presentation;

const mode = process.argv[2];
const input: unknown = JSON.parse(await new Response(Bun.stdin.stream()).text());
if (mode === 'presentations') {
  process.stdout.write(JSON.stringify((input as Inputs[]).map(presentationOf)));
} else if (mode === 'front-door') {
  const { contextHeader, platform, dashboard, profiles, antigravity } = input as {
    contextHeader: unknown;
    platform: string;
    dashboard: unknown;
    profiles: unknown;
    antigravity: unknown;
  };
  const ctx = parseHomeContext(contextHeader, platform);
  const envelope = ctx && Array.isArray(profiles) ? homeReadInput(ctx, dashboard, profiles, antigravity) : null;
  process.stdout.write(JSON.stringify({ envelope }));
} else {
  process.stderr.write('usage: envelope.ts presentations|front-door < input.json\n');
  process.exit(2);
}
