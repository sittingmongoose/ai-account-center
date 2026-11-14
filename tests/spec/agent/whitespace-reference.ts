/** Batch the property corpus through the real TS parser; no host probes. */
import fs from 'node:fs';
import { parseMachineProbe } from '../../../src/web-server/status/status-collectors';
const rows = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
fs.writeFileSync(
  process.argv[3],
  JSON.stringify(
    rows.map((r: { platform: 'linux' | 'mac' | 'windows'; probe: string }) =>
      parseMachineProbe(r.platform, r.probe)
    )
  )
);
