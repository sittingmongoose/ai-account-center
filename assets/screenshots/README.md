# AI Account Center screenshot provenance

The [dashboard](ai-account-center-dashboard.png) and
[Analytics](ai-account-center-analytics.png) previews were captured on
2026-10-01 from the compiled Slint 1.18.1 dashboard in headed Mac Chrome
using native WebGL. Dashboard viewport: 1920×1440 CSS pixels. Analytics viewport:
1920×2160 CSS pixels. Both used a DPR-2 renderer backbuffer; exported PNGs retain
the CSS viewport dimensions. No image generation or interface mockup was used.

The isolated loopback browser served validated public-response fixtures from
2026-10-01 15:15:58 UTC, covering 15 accounts, all nine providers and 69 usable
account/window histories. Account names, IDs, emails and source paths were
sanitized; every visible email uses `example.com`. Credentials were absent.
Metric, reset, expiration, status and observed-history fields retained their
source values. Those older fixtures have no Fable Max readings, so the new
slots remain unavailable. Muse shows its retained cached percentage reading with
the original 13:20:06 UTC sample time, rather than the later 15:15:19 UTC fetch.
Displayed dates use the browser's local timezone. The home cards use the compact
270-pixel layout; Analytics totals use compact labels with exact notes/tooltips.

Analytics displays actual earlier Ubuntu CLI activity from Claude Code and
Codex logs: input/output/cache tokens, session/parsed-entry counts, daily
API-equivalent cost estimates and model plots. Estimates are not subscription
charges or account billing attribution. Model token shares use the listed
models only. All histories keeps each account/window separate; gaps and reset
boundaries are preserved instead of connecting missing observations.

Captured source fingerprint:
`e7ecc939770daea2798ef7c9d857265cc4759047d64608dfb217689ac20b040e`.
Compiled WASM SHA-256:
`3ad6174f091f854015a64b075d87d20ae5ee0d41dba7db6b92c57420f5559ab3`.

| Screenshot | SHA-256 |
| --- | --- |
| Dashboard | `04077680de6085a9fdd68de7951d149da6da8681af2d97e20e5a9023e6a5e595` |
| Analytics | `8027c501a9f5a0eb5fa777a18cadb79d2ec3ff480f9d6425d4cd3bccfd42239f` |

Rendering completed without page errors. PNG text metadata is absent. The
isolated preview browser/profile was removed after capture. These screenshots
verify actual rendering with sanitized fixture responses; they do not establish
current live-provider availability, deployed-backend authentication or end-to-end
behavior. Private fixture responses, previous previews and detailed verification
receipts are retained outside the product checkout.
